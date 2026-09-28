"""Offline FLUX.2 [klein] 4B line-art worker for Diorama.

Loads a locally installed pipeline (see build-aux/setup-line-art.py) and never
touches the network: the Hugging Face offline switches are set before any
Hugging Face library is imported, and loading uses ``local_files_only``.

Memory: the text encoder (Qwen3-4B, ~8 GB bf16) and the transformer (~8 GB
bf16) never coexist. The fixed prompt is encoded once and its embeddings are
cached in the model directory; denoising loads only the transformer and VAE.
Every component is loaded straight onto the device (``device_map``), so
weights are not staged in system RAM.
"""

import argparse
import gc
import hashlib
import inspect
import json
import os
import sys

os.environ["HF_HUB_OFFLINE"] = "1"
os.environ["TRANSFORMERS_OFFLINE"] = "1"
# MIOpen's default benchmarking search fails every convolution on some ROCm
# GPUs (e.g. gfx1151: "Invalid elapsed time detected in EvaluateInvokers").
# The immediate-mode search is correct there and harmless elsewhere.
os.environ.setdefault("MIOPEN_FIND_MODE", "FAST")
# ROCm ships flash and memory-efficient attention for RDNA GPUs behind this
# switch. Without it, attention falls back to the math kernel, whose memory
# grows quadratically with the image and runs a 1024px edit out of memory.
os.environ.setdefault("TORCH_ROCM_AOTRITON_ENABLE_EXPERIMENTAL", "1")

SETUP = "python3 build-aux/setup-line-art.py"
REVISION_MARKER = ".diorama-revision"
PROMPT_CACHE_SCHEMA = "diorama-prompt-embeds-v1"
OUT_OF_MEMORY = (
    "the GPU ran out of memory. FLUX.2 [klein] 4B needs about 8 GB of "
    "GPU-addressable memory for each stage (prompt encoding, then denoising); "
    "close other GPU-heavy apps (such as local LLM servers) and retry"
)


def fail(message):
    print(f"line-art worker: {message}", file=sys.stderr, flush=True)
    sys.exit(2)


def arguments():
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--prompt", required=True)
    parser.add_argument("--seed", type=int, required=True)
    parser.add_argument("--steps", type=int, required=True)
    parser.add_argument("--guidance", type=float, required=True)
    parser.add_argument("--device", choices=("cuda", "cpu"))
    return parser.parse_args()


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


def encoding_parameters(pipeline_class):
    """The pipeline's own prompt-encoding defaults (sequence length and the
    text-encoder layers it stacks), so they are not duplicated here."""
    parameters = inspect.signature(pipeline_class.encode_prompt).parameters
    return {
        "max_sequence_length": parameters["max_sequence_length"].default,
        "text_encoder_out_layers": tuple(parameters["text_encoder_out_layers"].default),
    }


def prompt_cache_path(pipeline_class, model, prompt):
    """Embeddings depend on the weights, prompt, and encoding parameters."""
    with open(os.path.join(model, REVISION_MARKER), encoding="utf-8") as marker:
        revision = marker.read().strip()
    encoding = encoding_parameters(pipeline_class)
    key = json.dumps(
        [
            PROMPT_CACHE_SCHEMA,
            revision,
            prompt,
            encoding["max_sequence_length"],
            list(encoding["text_encoder_out_layers"]),
            "bfloat16",
        ]
    )
    digest = hashlib.sha256(key.encode("utf-8")).hexdigest()
    return os.path.join(model, f".diorama-prompt-{digest}.safetensors")


def release(torch):
    gc.collect()
    if torch.cuda.is_available():
        torch.cuda.empty_cache()


def encode_prompt(torch, pipeline_class, model, prompt, device):
    """Load only the tokenizer and text encoder, directly onto the device."""
    pipeline = pipeline_class.from_pretrained(
        model,
        transformer=None,
        vae=None,
        dtype=torch.bfloat16,
        device_map=device,
        local_files_only=True,
    )
    with torch.inference_mode():
        prompt_embeds, _text_ids = pipeline.encode_prompt(
            prompt=prompt, device=device, **encoding_parameters(pipeline_class)
        )
    prompt_embeds = prompt_embeds.to("cpu", torch.bfloat16).contiguous()
    del pipeline
    release(torch)
    return prompt_embeds


def cached_prompt_embeds(torch, pipeline_class, model, prompt, device):
    from safetensors.torch import load_file, save_file

    path = prompt_cache_path(pipeline_class, model, prompt)
    if os.path.isfile(path):
        return load_file(path)["prompt_embeds"]
    prompt_embeds = encode_prompt(torch, pipeline_class, model, prompt, device)
    temporary = f"{path}.{os.getpid()}.tmp"
    try:
        save_file({"prompt_embeds": prompt_embeds}, temporary, metadata={"prompt": prompt})
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)
    return prompt_embeds


def generate(torch, pipeline_class, args, source, device):
    prompt_embeds = cached_prompt_embeds(torch, pipeline_class, args.model, args.prompt, device)
    # Transformer and VAE only; text_ids are recomputed from the embeddings.
    pipeline = pipeline_class.from_pretrained(
        args.model,
        text_encoder=None,
        tokenizer=None,
        dtype=torch.bfloat16,
        device_map=device,
        local_files_only=True,
    )
    pipeline.vae.enable_tiling()
    pipeline.set_progress_bar_config(disable=True)
    width, height = source.size
    generator = torch.Generator(device=device).manual_seed(args.seed)
    with torch.inference_mode():
        return pipeline(
            image=source,
            prompt_embeds=prompt_embeds.to(device),
            height=height,
            width=width,
            num_inference_steps=args.steps,
            guidance_scale=args.guidance,
            generator=generator,
        ).images[0]


def main():
    args = arguments()
    if not os.path.isfile(os.path.join(args.model, "model_index.json")):
        fail(f"no FLUX.2 [klein] pipeline at {args.model}; run {SETUP}")
    try:
        import torch
        from PIL import Image
        from diffusers import Flux2KleinPipeline
        from diffusers.utils import logging as diffusers_logging
        from transformers.utils import logging as transformers_logging
    except ImportError as error:
        fail(f"missing Python package ({error}); run {SETUP} to create the line-art environment")
    # Keep the log tail Diorama reports readable.
    diffusers_logging.disable_progress_bar()
    transformers_logging.disable_progress_bar()

    source = Image.open(args.image).convert("RGB")
    width, height = source.size
    if width % 16 or height % 16:
        fail(f"input {width}x{height} is not a multiple of 16")
    device = device_for(torch, args.device)
    try:
        result = generate(torch, Flux2KleinPipeline, args, source, device)
    except torch.OutOfMemoryError:
        fail(OUT_OF_MEMORY)
    line_art = result.convert("L")
    if line_art.size != source.size:
        fail(f"the pipeline returned {line_art.size[0]}x{line_art.size[1]} for a {width}x{height} input")
    line_art.save(args.output, format="PNG")


if __name__ == "__main__":
    main()
