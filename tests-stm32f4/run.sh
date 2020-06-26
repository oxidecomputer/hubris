#!/bin/bash

set -euo pipefail

./package.sh "$@"
arm-none-eabi-gdb -q \
  -x target/packager/script.gdb \
  -x openocd.gdb \
  target/packager/combined.elf 
