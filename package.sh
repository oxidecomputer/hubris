#!/bin/bash

set -euo pipefail

cargo xtask packager demo/app.toml
arm-none-eabi-objcopy -Isrec -O elf32-littlearm target/packager/combined.{srec,elf}
