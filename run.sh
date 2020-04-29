#!/bin/bash

set -euo pipefail

mkdir -p target/packager
cargo run --bin packager -- "$@" demo/app.toml target/packager
arm-none-eabi-objcopy -Isrec -O elf32-littlearm target/packager/combined.{srec,elf}
arm-none-eabi-gdb -q \
  -x target/packager/script.gdb \
  -x openocd.gdb \
  target/packager/combined.elf 
