#!/bin/bash

set -euo pipefail

cargo xtask dist demo/app.toml
arm-none-eabi-gdb -q \
  -x target/dist/script.gdb \
  -x openocd.gdb \
  target/dist/combined.elf 
