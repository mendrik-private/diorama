"""Offline FLUX.2 [klein] 9B Game Asset worker for Diorama.

Two modes, run as separate processes so that the memory of one is returned
to the system before the other starts:

``--encode`` loads the text encoder (Qwen3-8B, truncated to the layers FLUX
reads) on the CPU in bf16, encodes each given prompt, saves its embeddings to
the given file and exits. Encoding runs on the CPU because loading the
encoder onto the GPU stages its weights through system memory twice.

``--serve`` loads the Q4_K_M GGUF transformer and the VAE straight onto the
GPU once and then stays resident, running one job at a time. Each job is one
JSON object per line on stdin; it generates, one after the other, line art
and a de-inked fill at the requested size from one reference image and the
prompts' saved embeddings:

    {"job": 1, "image": "...", "width": 512, "height": 512, "steps": 2,
     "guidance": 1.0, "line_art_embeds": "...", "line_art_seed": 0,
     "line_art_output": "...", "fill_embeds": "...", "fill_seed": 0,
     "fill_output": "..."}

``{"cancel": 1}`` stops that job at its next denoising step. The worker exits
when stdin closes or after ``--idle-timeout`` seconds without a job.

Everything comes from files the user installed with
build-aux/setup-line-art.py; the worker never touches the network: the
Hugging Face offline switches are set before any Hugging Face library is
imported, and loading uses ``local_files_only``.

Progress is streamed as one JSON object per line on stdout. Everything else,
including library output, goes to stderr (Diorama's worker log):

    {"event": "stage", "stage": "encode", "prompts": 2}
    {"event": "stage", "stage": "load"}
    {"event": "ready"}
    {"event": "step", "job": 1, "image": "line_art", "step": 1, "steps": 2, "elapsed": 4.3}
    {"event": "stage", "stage": "decode", "job": 1, "image": "line_art"}
    {"event": "done", "job": 1}
    {"event": "error", "job": 1, "message": "..."}
    {"event": "cancelled", "job": 1}

``elapsed`` is the time in seconds since that image's generation started.
"""

import argparse
import json
import os
import queue
import sys
import threading
import time
import traceback

os.environ["HF_HUB_OFFLINE"] = "1"
os.environ["TRANSFORMERS_OFFLINE"] = "1"
# MIOpen's default benchmarking search fails every convolution on some ROCm
# GPUs (e.g. gfx1151: "Invalid elapsed time detected in EvaluateInvokers").
# The immediate-mode search is correct there and harmless elsewhere.
os.environ.setdefault("MIOPEN_FIND_MODE", "FAST")
# ROCm ships flash and memory-efficient attention for RDNA GPUs behind this
# switch. Without it, attention falls back to the math kernel, whose memory
# grows quadratically with the image.
os.environ.setdefault("TORCH_ROCM_AOTRITON_ENABLE_EXPERIMENTAL", "1")

SETUP = "python3 build-aux/setup-line-art.py"
OUT_OF_MEMORY = (
    "the GPU ran out of memory. FLUX.2 [klein] 9B needs about 7.5 GB of "
    "GPU-addressable memory to generate at 512 pixels and more for larger "
    "sizes; close other GPU-heavy apps (such as local LLM servers) and retry"
)


def protocol_stream():
    """Keep the real stdout for progress events and send everything else,
    including output from native libraries, to stderr."""
    sys.stdout.flush()
    stream = os.fdopen(os.dup(1), "w", encoding="utf-8")
    os.dup2(2, 1)
    sys.stdout = sys.stderr
    return stream


PROTOCOL = protocol_stream()


def emit(event, **fields):
    PROTOCOL.write(json.dumps({"event": event, **fields}) + "\n")
    PROTOCOL.flush()


def fail(message):
    print(f"line-art worker: {message}", file=sys.stderr, flush=True)
    sys.exit(2)


def arguments():
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", required=True)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--encode", action="store_true", help="only encode prompts, on the CPU")
    mode.add_argument("--serve", action="store_true", help="run jobs from stdin on the GPU")
    # Encode mode.
    parser.add_argument(
        "--prompt-embeds", nargs=2, action="append", metavar=("PROMPT", "PATH"), default=[]
    )
    parser.add_argument("--text-encoder-layers")
    parser.add_argument("--max-sequence-length", type=int)
    # Serve mode.
    parser.add_argument("--gguf")
    parser.add_argument("--idle-timeout", type=float)
    parser.add_argument("--device", choices=("cuda", "cpu"))
    args = parser.parse_args()
    required = (
        ("prompt_embeds", "text_encoder_layers", "max_sequence_length")
        if args.encode
        else ("gguf", "idle_timeout")
    )
    missing = [name for name in required if getattr(args, name) in (None, [])]
    if missing:
        parser.error("missing " + ", ".join("--" + name.replace("_", "-") for name in missing))
    return args


def imports():
    try:
        import torch
        from diffusers import Flux2KleinPipeline
        from diffusers.utils import logging as diffusers_logging
        from transformers.utils import logging as transformers_logging
    except ImportError as error:
        fail(f"missing Python package ({error}); run {SETUP} to create the line-art environment")
    # Keep the log tail Diorama reports readable.
    diffusers_logging.disable_progress_bar()
    transformers_logging.disable_progress_bar()
    return torch, Flux2KleinPipeline


def encode(args):
    torch, Flux2KleinPipeline = imports()
    from safetensors.torch import save_file
    from transformers import AutoConfig, Qwen3ForCausalLM

    layers = tuple(int(layer) for layer in args.text_encoder_layers.split(","))
    emit("stage", stage="encode", prompts=len(args.prompt_embeds))
    text_encoder = os.path.join(args.model, "text_encoder")
    config = AutoConfig.from_pretrained(text_encoder, local_files_only=True)
    # FLUX reads only these hidden layers; the ones above are never loaded.
    config.num_hidden_layers = max(layers) + 1
    model = Qwen3ForCausalLM.from_pretrained(
        text_encoder, config=config, dtype=torch.bfloat16, device_map="cpu", local_files_only=True
    )
    pipeline = Flux2KleinPipeline.from_pretrained(
        args.model,
        transformer=None,
        vae=None,
        text_encoder=model,
        dtype=torch.bfloat16,
        local_files_only=True,
    )
    with torch.inference_mode():
        for prompt, path in args.prompt_embeds:
            embeds, _text_ids = pipeline.encode_prompt(
                prompt=prompt,
                device="cpu",
                max_sequence_length=args.max_sequence_length,
                text_encoder_out_layers=layers,
            )
            temporary = f"{path}.{os.getpid()}.tmp"
            try:
                save_file(
                    {"prompt_embeds": embeds.to(torch.bfloat16).contiguous()},
                    temporary,
                    metadata={"prompt": prompt},
                )
                os.replace(temporary, path)
            finally:
                if os.path.exists(temporary):
                    os.unlink(temporary)


def device_for(torch, requested):
    if requested == "cpu":
        return "cpu"
    if torch.cuda.is_available():
        return "cuda"
    if requested == "cuda":
        fail("--device cuda was requested but PyTorch sees no GPU")
    fail(
        "a GPU is required: this PyTorch build sees no CUDA/ROCm device. "
        "Install a GPU-enabled PyTorch for the host Python and rerun "
        f"{SETUP}, or set DIORAMA_LINE_ART_DEVICE=cpu to accept very slow CPU inference"
    )


def load_pipeline(torch, Flux2KleinPipeline, args, device):
    """The GGUF transformer and the pipeline's VAE, directly on the device."""
    from diffusers import Flux2Transformer2DModel, GGUFQuantizationConfig

    emit("stage", stage="load")
    transformer = Flux2Transformer2DModel.from_single_file(
        args.gguf,
        quantization_config=GGUFQuantizationConfig(compute_dtype=torch.bfloat16),
        config=args.model,
        subfolder="transformer",
        dtype=torch.bfloat16,
        device=device,
        local_files_only=True,
    )
    # text_ids are recomputed from the saved embeddings.
    pipeline = Flux2KleinPipeline.from_pretrained(
        args.model,
        transformer=transformer,
        text_encoder=None,
        tokenizer=None,
        dtype=torch.bfloat16,
        device_map=device,
        local_files_only=True,
    )
    pipeline.set_progress_bar_config(disable=True)
    return pipeline


class JobCancelled(Exception):
    pass


def generate_image(torch, pipeline, job, name, reference, embeds, seed, device, cancelled):
    started = time.monotonic()
    steps = job["steps"]

    def step_end(_pipeline, index, _timestep, callback_kwargs):
        step = index + 1
        if device == "cuda":
            torch.cuda.synchronize()
        emit(
            "step",
            job=job["job"],
            image=name,
            step=step,
            steps=steps,
            elapsed=round(time.monotonic() - started, 3),
        )
        if job["job"] in cancelled:
            raise JobCancelled()
        if step == steps:
            emit("stage", stage="decode", job=job["job"], image=name)
        return callback_kwargs

    generator = torch.Generator(device=device).manual_seed(seed)
    with torch.inference_mode():
        return pipeline(
            image=reference,
            prompt_embeds=embeds.to(device),
            height=job["height"],
            width=job["width"],
            num_inference_steps=steps,
            guidance_scale=job["guidance"],
            generator=generator,
            callback_on_step_end=step_end,
        ).images[0]


def run_job(torch, pipeline, job, device, cancelled):
    from PIL import Image
    from safetensors.torch import load_file

    width, height = job["width"], job["height"]
    if width <= 0 or height <= 0 or width % 16 or height % 16:
        raise ValueError(f"output {width}x{height} is not a positive multiple of 16")
    reference = Image.open(job["image"]).convert("RGB")
    for name, mode in (("line_art", "L"), ("fill", "RGB")):
        if job["job"] in cancelled:
            raise JobCancelled()
        embeds = load_file(job[f"{name}_embeds"])["prompt_embeds"]
        result = generate_image(
            torch, pipeline, job, name, reference, embeds, job[f"{name}_seed"], device, cancelled
        )
        if result.size != (width, height):
            raise ValueError(f"the pipeline returned {result.size[0]}x{result.size[1]} for {width}x{height}")
        result.convert(mode).save(job[f"{name}_output"], format="PNG")


def read_messages(jobs, cancelled):
    """Queue jobs and record cancellations as they arrive; None marks the end
    of stdin."""
    for line in sys.stdin:
        try:
            message = json.loads(line)
        except ValueError:
            print(f"line-art worker: ignoring malformed input: {line!r}", file=sys.stderr, flush=True)
            continue
        if "cancel" in message:
            cancelled.add(message["cancel"])
        else:
            jobs.put(message)
    jobs.put(None)


def shut_down(status=0):
    # Skip the interpreter teardown, which takes about a second with ROCm.
    sys.stderr.flush()
    os._exit(status)


def serve(args):
    if not os.path.isfile(args.gguf):
        fail(f"no FLUX.2 [klein] GGUF transformer at {args.gguf}; run {SETUP}")
    torch, Flux2KleinPipeline = imports()
    device = device_for(torch, args.device)
    try:
        pipeline = load_pipeline(torch, Flux2KleinPipeline, args, device)
    except torch.OutOfMemoryError:
        fail(OUT_OF_MEMORY)
    emit("ready")
    jobs, cancelled = queue.Queue(), set()
    threading.Thread(target=read_messages, args=(jobs, cancelled), daemon=True).start()
    while True:
        try:
            job = jobs.get(timeout=args.idle_timeout)
        except queue.Empty:
            print("line-art worker: idle, exiting", file=sys.stderr, flush=True)
            shut_down()
        if job is None:
            shut_down()
        try:
            run_job(torch, pipeline, job, device, cancelled)
        except JobCancelled:
            emit("cancelled", job=job["job"])
        except torch.OutOfMemoryError:
            # A fragmented allocator is best recovered by a fresh process.
            emit("error", job=job["job"], message=OUT_OF_MEMORY)
            shut_down(3)
        except Exception as error:  # noqa: BLE001 - reported to Diorama per job
            traceback.print_exc(file=sys.stderr)
            emit("error", job=job["job"], message=f"{type(error).__name__}: {error}")
        else:
            emit("done", job=job["job"])
        cancelled.discard(job["job"])


def main():
    args = arguments()
    if not os.path.isfile(os.path.join(args.model, "model_index.json")):
        fail(f"no FLUX.2 [klein] pipeline at {args.model}; run {SETUP}")
    if args.encode:
        encode(args)
        shut_down()
    serve(args)


if __name__ == "__main__":
    main()
