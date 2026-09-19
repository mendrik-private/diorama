# Native BiRefNet model bundle

Diorama's cutout worker uses the native BiRefNet GGUF model. A release bundle
contains `BiRefNet-F16.gguf` and `manifest.json`; it installs to
`/app/share/diorama/models`. The tagged-release workflow fetches the pinned
GGUF, verifies SHA-256
`5d5fd824c8fb2c1a65fc4345458b2e78777d949418385ea7bba5a9f104364d77`, then
creates that bundle. The installed application never downloads the model.

For a source build, make the same verified bundle from an audited local GGUF:

```sh
python3 build-aux/prepare-native-model-bundle.py \
  --birefnet /secure-inputs/BiRefNet-F16.gguf \
  --dest release-models
```

The command refuses an existing destination, validates the input and copied
file hashes, and writes the checked manifest. Configure Meson with
`-Dnative_model_bundle=/absolute/path/to/release-models` to install it. Clean
source builds leave this option empty and require no model artifact.

At runtime `DIORAMA_BIREFNET_MODEL` selects one absolute GGUF path. Otherwise,
`DIORAMA_MODELS_DIR` selects an absolute directory containing
`BiRefNet-F16.gguf`; then
Diorama checks `$XDG_DATA_HOME/diorama/models`,
`$HOME/.local/share/diorama/models`, a sibling `share/diorama/models` next to
its executable, and `/app/share/diorama/models`.

Content-aware fill remains the documented local Python/Torch LaMa setup in the
[README](../README.md#local-content-aware-fill-and-cutouts).
