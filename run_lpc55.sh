#!/bin/bash

set -euo pipefail

cargo xtask dist lpc55/app.toml
arm-none-eabi-gdb \
  -x target/dist/script.gdb \
  -x lpc55.gdb \
  target/dist/combined.elf 
