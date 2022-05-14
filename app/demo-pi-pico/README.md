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
uf2l pack $RUSTBOOT/elf/rustboot-w25q080 \
          target/demo-pi-pico/dist/combined.elf \
          hubris-pico.uf2
```

Then, hold BOOTSEL while plugging in your Pi Pico. It should show up as a USB
drive. Copy the `hubris-pico.uf2` file onto it. It should reboot and begin
blinking a light at 1 Hz.
