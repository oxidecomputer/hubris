#!/bin/bash

set -euo pipefail

cargo xtask packager lpc55/app.toml
arm-none-eabi-gdb \
  -x target/packager/script.gdb \
  -x lpc55.gdb \
  target/packager/combined.elf 
