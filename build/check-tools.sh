#!/usr/bin/env bash
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.

# Script to `cargo check` every host tool in the `tools/` folder.
#
# These tools are expected to be built for the host system, with no `--target`
# flag set.
#
# Rather than hardcoding the list of tools (inevitably forgetting to add some),
# we ask cargo for every workspace package whose manifest lives under `tools/`.
# Any extra arguments are passed through to `cargo check`. Example usage:
#
# ```sh
# ./build/check-tools.sh --locked
# ```

set -euo pipefail

cd "$(dirname "$0")/.."

# Makes a bash array called `tools` that contains the name of every package that
# has a `manifest_path` that starts with `$PWD/tools/`. These are the packages
# that we will check with cargo.
#
# `mapfile -t` works kind of like `input.lines().collect::<Vec<_>>()` in Rust.
mapfile -t tools < <(
    # Prints each name that was found with the filter as a single line.
    cargo metadata --format-version 1 --no-deps \
        | jq -r --arg root "$PWD/tools/" \
            '.packages[] | select(.manifest_path | startswith($root)) | .name'
)

# If we have no tools, something has gone wrong.
if (( ${#tools[@]} == 0 )); then
    echo "error: no packages found under tools/" >&2
    exit 1
fi

echo "Checking: ${tools[*]}"

# Echo out the invocation below in case we ever need to debug it.
set -x

# Check all tools in one invocation of `cargo check`, e.g.
#
# ```sh
# cargo check --package=one --package=two --locked
# ```
cargo check "${tools[@]/#/--package=}" "$@"
