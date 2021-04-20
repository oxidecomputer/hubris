# Gemini Bringup Board STM32H7 Bare Metal

This provides a template you can use for writing bare metal applications for the
service processor on the Gemini Bringup board (or compatible variants such as
the Gimletlet).

Pros:

- You can peek and poke whatever, without the OS having opinions.
- You don't necessarily need to understand Hubris to twiddle some GPIOs.
- You can still use some of our standard drivers (the ones that have a
  driver-server split).
- You can port over and play with open-source Rust or C code that assumes
  privileged mode and no memory protection, before porting it to a proper
  isolated driver.

Cons:

- No multitasking / RTOS features.
- No OS features (test framework, logging, etc.).
- No memory protection or crash robustness.
- No Humility support (GDB only).

## Hacking on this

All the commands shown below must be run from the same directory as this README
file.

To build and flash, in one terminal, run

```shell
openocd
```

and then in another, run

```shell
cargo run
```

## Making a new directory

If you'd like to save your work, you'll want to do it in a new directory.

```shell
cp -R gemini-bu-bare SOMENAMEHERE
vi gemini-bu-bare/Cargo.toml # change two instances of gemini-bu-bare
vi Cargo.toml # find gemini-bu-bare in list, add SOMENAMEHERE below
vi SOMENAMEHERE/.cargo/config # change one instance of gemini-bu-bare
```
