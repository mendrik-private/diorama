#!/bin/sh
set -eu

source_dir=$1
output=$2
build_type=$3
target_dir=${MESON_BUILD_ROOT:-"$source_dir/target/meson"}/cargo-target

if [ "$build_type" = "test" ]; then
  exec env CARGO_TARGET_DIR="$target_dir" cargo test --manifest-path "$source_dir/Cargo.toml"
fi

profile=
if [ "$build_type" = "release" ] || [ "$build_type" = "minsize" ]; then
  profile=--release
fi

env CARGO_TARGET_DIR="$target_dir" cargo build --manifest-path "$source_dir/Cargo.toml" $profile
if [ -n "$output" ]; then
  if [ -n "$profile" ]; then
    cp "$target_dir/release/diorama" "$output"
  else
    cp "$target_dir/debug/diorama" "$output"
  fi
fi

