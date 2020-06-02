#!/bin/bash

set -euo pipefail

./package_lpc55.sh "$@"
arm-none-eabi-gdb \
  -x target/packager/script.gdb \
  -x lpc55.gdb \
  target/packager/combined.elf 
