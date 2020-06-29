#!/bin/bash

set -euo pipefail

cargo xtask packager demo/app.toml
arm-none-eabi-gdb -q \
  -x target/packager/script.gdb \
  -x openocd.gdb \
  target/packager/combined.elf 
