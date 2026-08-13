#!/bin/sh
set -eu

project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$project_root"

cargo test -- --ignored --list \
  | sed -n 's/: test$//p' \
  | while IFS= read -r test_name; do
      cargo test "$test_name" -- --ignored --exact --test-threads=1
    done
