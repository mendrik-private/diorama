# BiRefNet parity fixtures

`elf-1024.png` and `selection-1024.png` are fixed-size inputs.  `selection-1280x800.png` covers the public non-square resize path.  The corresponding `vision-*.png` files were produced with vision.cpp's CPU backend and the F16 GGUF whose SHA-256 is `5d5fd824c8fb2c1a65fc4345458b2e78777d949418385ea7bba5a9f104364d77`.

`elf-1024.png` was made from `src/tools/scale/game_asset/fixtures/elf.png` with a fixed 1024×1024 resize. `selection-1280x800.png` is copied from `data/screenshots/selection.png`; `selection-1024.png` is its fixed-size parity input.

The reference command was:

```sh
vision-cli birefnet -m BiRefNet-F16.gguf -i INPUT.png -o OUTPUT.png -b cpu
```

vision.cpp resizes colour input with STB's alpha-aware sRGB Catmull-Rom/Mitchell defaults and resizes float masks linearly. The ignored native parity tests require `DIORAMA_BIREFNET_MODEL`. The 1024×1024 fixtures require MAE below 0.3 alpha units and threshold IoU above 0.998. The non-square fixture requires MAE below 0.5 and IoU above 0.996: its native and STB-prepared inputs differ by at most one 8-bit channel unit, which the model amplifies slightly.
