#!/bin/bash

set -euo pipefail

mkdir -p target/packager
cargo run -p packager --bin packager -- "$@" app.toml target/packager
arm-none-eabi-objcopy -Isrec -O elf32-littlearm target/packager/combined.{srec,elf}
