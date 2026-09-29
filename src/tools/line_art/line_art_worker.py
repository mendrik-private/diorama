"""Offline FLUX.2 [klein] 9B Game Asset worker for Diorama.

Two modes, run as separate processes so that the memory of one is returned
to the system before the other starts:

``--encode`` loads the text encoder (Qwen3-8B, truncated to the layers FLUX
reads) on the CPU in bf16, encodes each given prompt and saves its embeddings
to the given file. Encoding runs on the CPU because loading the encoder onto
the GPU stages its weights through system memory twice.

The default mode loads the Q4_K_M GGUF transformer and the VAE straight onto
the GPU and generates, one after the other on that load, line art and a
de-inked fill at the requested size from one reference image and the
prompts' saved embeddings.

Everything comes from files the user installed with
build-aux/setup-line-art.py; the worker never touches the network: the
Hugging Face offline switches are set before any Hugging Face library is
imported, and loading uses ``local_files_only``.

Progress is streamed as one JSON object per line on stdout. Everything else,
including library output, goes to stderr (Diorama's worker log):

    {"event": "stage", "stage": "encode", "prompts": 2}
    {"event": "stage", "stage": "load"}
    {"event": "step", "image": "line_art", "step": 1, "steps": 4, "elapsed": 1.9}
    {"event": "stage", "stage": "decode", "image": "line_art"}
    {"event": "done"}

``elapsed`` is the time in seconds since that image's generation started.
"""

import argparse
import json
import os
import sys
import time

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
    parser.add_argument("--encode", action="store_true", help="only encode prompts, on the CPU")
    # Encode mode.
    parser.add_argument(
        "--prompt-embeds", nargs=2, action="append", metavar=("PROMPT", "PATH"), default=[]
    )
    parser.add_argument("--text-encoder-layers")
    parser.add_argument("--max-sequence-length", type=int)
    # Generate mode.
    parser.add_argument("--gguf")
    parser.add_argument("--image")
    parser.add_argument("--width", type=int)
    parser.add_argument("--height", type=int)
    parser.add_argument("--line-art-embeds")
    parser.add_argument("--line-art-seed", type=int)
    parser.add_argument("--line-art-output")
    parser.add_argument("--fill-embeds")
    parser.add_argument("--fill-seed", type=int)
    parser.add_argument("--fill-output")
    parser.add_argument("--steps", type=int)
    parser.add_argument("--guidance", type=float)
    parser.add_argument("--device", choices=("cuda", "cpu"))
    args = parser.parse_args()
    required = (
        ("prompt_embeds", "text_encoder_layers", "max_sequence_length")
        if args.encode
        else (
            "gguf", "image", "width", "height", "line_art_embeds", "line_art_seed",
            "line_art_output", "fill_embeds", "fill_seed", "fill_output", "steps", "guidance",
        )
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


def generate_image(torch, pipeline, name, reference, embeds, seed, args, device):
    started = time.monotonic()

    def step_end(_pipeline, index, _timestep, callback_kwargs):
        step = index + 1
        if device == "cuda":
            torch.cuda.synchronize()
        emit(
            "step",
            image=name,
            step=step,
            steps=args.steps,
            elapsed=round(time.monotonic() - started, 3),
        )
        if step == args.steps:
            emit("stage", stage="decode", image=name)
        return callback_kwargs

    generator = torch.Generator(device=device).manual_seed(seed)
    with torch.inference_mode():
        return pipeline(
            image=reference,
            prompt_embeds=embeds.to(device),
            height=args.height,
            width=args.width,
            num_inference_steps=args.steps,
            guidance_scale=args.guidance,
            generator=generator,
            callback_on_step_end=step_end,
        ).images[0]


def generate(args):
    if args.width <= 0 or args.height <= 0 or args.width % 16 or args.height % 16:
        fail(f"output {args.width}x{args.height} is not a positive multiple of 16")
    if not os.path.isfile(args.gguf):
        fail(f"no FLUX.2 [klein] GGUF transformer at {args.gguf}; run {SETUP}")
    torch, Flux2KleinPipeline = imports()
    from PIL import Image
    from safetensors.torch import load_file

    reference = Image.open(args.image).convert("RGB")
    device = device_for(torch, args.device)
    images = (
        ("line_art", args.line_art_embeds, args.line_art_seed, args.line_art_output, "L"),
        ("fill", args.fill_embeds, args.fill_seed, args.fill_output, "RGB"),
    )
    embeds = {name: load_file(path)["prompt_embeds"] for name, path, *_ in images}
    try:
        pipeline = load_pipeline(torch, Flux2KleinPipeline, args, device)
        for name, _path, seed, output, mode in images:
            result = generate_image(
                torch, pipeline, name, reference, embeds[name], seed, args, device
            )
            if result.size != (args.width, args.height):
                fail(
                    f"the pipeline returned {result.size[0]}x{result.size[1]} "
                    f"for {args.width}x{args.height}"
                )
            result.convert(mode).save(output, format="PNG")
    except torch.OutOfMemoryError:
        fail(OUT_OF_MEMORY)


def main():
    args = arguments()
    if not os.path.isfile(os.path.join(args.model, "model_index.json")):
        fail(f"no FLUX.2 [klein] pipeline at {args.model}; run {SETUP}")
    if args.encode:
        encode(args)
    else:
        generate(args)
        emit("done")
    # The outputs are saved and the streams flushed; skip the interpreter
    # teardown, which takes about a second with ROCm.
    sys.stderr.flush()
    os._exit(0)


if __name__ == "__main__":
    main()
