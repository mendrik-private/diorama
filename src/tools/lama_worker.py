"""Diorama's offline LaMa worker; embedded in the Rust binary.

Uses the TorchScript big-lama export published by Sanster/models, with RGB
float32 NCHW input and a binary N1HW removal mask. No downloads during editing.
"""

import argparse
import os


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--model", required=True)
    parser.add_argument("--image", required=True)
    parser.add_argument("--mask", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--device", choices=("cpu", "cuda"), default="cpu")
    args = parser.parse_args()

    import numpy as np
    import torch
    from PIL import Image

    torch.set_num_threads(min(4, os.cpu_count() or 1))
    source = Image.open(args.image).convert("RGB")
    mask = Image.open(args.mask).convert("L")
    if source.size != mask.size:
        raise ValueError("LaMa image and mask dimensions differ")
    original_size = source.size
    # Bound inference memory on laptops. Only synthesized pixels are later
    # merged into the original-resolution canvas by Rust.
    scale = min(1.0, 1024 / max(source.size))
    size = tuple(max(1, round(d * scale)) for d in source.size)
    if size != source.size:
        source = source.resize(size, Image.Resampling.LANCZOS)
        # BOX plus >0 retains small masked features when reducing resolution.
        mask = mask.resize(size, Image.Resampling.BOX)
    rgb = np.asarray(source, dtype=np.float32) / 255.0
    removal = (np.asarray(mask) > 0).astype(np.float32)
    height, width = removal.shape
    padding = ((0, (-height) % 8), (0, (-width) % 8))
    rgb = np.pad(rgb, (*padding, (0, 0)), mode="symmetric")
    removal = np.pad(removal, padding, mode="symmetric")
    pixels = torch.from_numpy(np.ascontiguousarray(rgb.transpose(2, 0, 1)))[None]
    matte = torch.from_numpy(np.ascontiguousarray(removal))[None, None]
    model = torch.jit.load(args.model, map_location=args.device).eval()
    with torch.inference_mode():
        result = model(pixels.to(args.device), matte.to(args.device))
    result = result[0, :, :height, :width].permute(1, 2, 0).cpu().numpy()
    if result.shape != (height, width, 3) or not np.isfinite(result).all():
        raise ValueError("LaMa returned an invalid image")
    output = Image.fromarray(np.clip(result * 255, 0, 255).astype(np.uint8))
    if output.size != original_size:
        output = output.resize(original_size, Image.Resampling.LANCZOS)
    output.save(args.output)


if __name__ == "__main__":
    main()
