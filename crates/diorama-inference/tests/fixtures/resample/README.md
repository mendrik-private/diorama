# STB resampling reference data

`stb_resample_probe.c` is a development-only generator for the literal
expectations in `src/resample.rs`. It is compiled against the pinned
`stb_image_resize.h` vendored by vision.cpp; it is not part of Diorama's
runtime or build. The probe covers an asymmetric 3×2 RGBA image with zero,
partial, and opaque alpha plus a scalar float mask with overshoot-producing
cubic weights.

Reference header SHA-256: `6a0e75adbabb48df9031c2e39ccd97437bb226fe31e7d4a01c7bf70a18d32ec6`.
Generation command: `cc -O2 stb_resample_probe.c -I /path/to/stb -lm -o probe && ./probe`.
