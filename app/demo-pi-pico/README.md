# Rasperry Pi Pico demo application

This will blink an LED, and not a whole lot else, on a Raspberry Pi Pico board
(based on the RP2040).

Currently, our tools don't know how to flash this board. So, to use this, you'll
have to jump through some hoops.

You will need:

- [uf2l](https://github.com/cbiffle/uf2l) - you can install it, or run it from
  the build directory, but the instructions below will just refer to it as
  `uf2l`.
- [rp2040-rustboot](https://github.com/cbiffle/rp2040-rustboot) - run the
  `build-all.sh` to produce ELF binaries. The instructions below will refer to
  its location as `$RUSTBOOT`.

To build and prepare the image:

```
cargo xtask dist app/demo-pi-pico/app.toml
uf2l pack -e 4096 \
          $RUSTBOOT/elf/rustboot-w25q080 \
          target/demo-pi-pico/dist/combined.elf \
          hubris-pico.uf2
```

Then, hold BOOTSEL while plugging in your Pi Pico. It should show up as a USB
drive. Copy the `hubris-pico.uf2` file onto it. It should reboot and begin
blinking a light at 1 Hz.

## Debugging

General RP2040 support in OpenOCD hasn't been upstreamed, but amusingly there
_is_ upstream support for the [pico-debug] same-chip debugger. This will run on
core 1 while Hubris runs on core 0.

To use this:

1. Flash Hubris as described above.
2. Ensure that you've got a fairly recent OpenOCD git build.
3. Download the GIMMECACHE version of the debug image.
4. Hold BOOTSEL and reboot your RP2040 board into the bootloader.
5. Copy the GIMMECACHE UF2 image onto the board. It should reboot. It does not
   appear to correctly start the Flash image by default, but that's easy to fix:
6. From this directory, run: `openocd -f openocd-pico-debug.cfg -c init -c reset
   -c exit`

The clock configuration under pico-debug is slightly different from the reset
configuration; there is code in `src/main.rs` to adjust for this. It is a hack.

[pico-debug]: https://github.com/majbthrd/pico-debug
