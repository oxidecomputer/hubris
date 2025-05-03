# LPC55 GPIO Pin configuration in `app.toml` files

Configuring LPC55 GPIO pins may require referencing the NXP UM11126 document and board schematics.

Some tasks, like `user_leds`, program the GPIO pins directly, but most
will use the API defined in `idl/lpc55-pins.idl`.

Tasks that use GPIO pins need to include a `[tasks.TASKNAME.config]` section.

For example, a `button` task in `app/lpc55xpresso/app-button.toml`
might use the red, green, and blue LEDs on the `LPCxpresso` board as well as the
user button. The LEDs would be attached to GPIOs configured as outputs while
the "user" button would be configured as an input with an interrupt.

The LPC55 GPIO Pin INTerrupts index is selected from the range `pint.irq0`
to `pint.irq7` and cannot be used more than once in the `app.toml`. 
The task stanza's `interrupts` and the pin configuration information need to agree on the `PINT` index.

```toml
[tasks.button]
# ...
interrupts = { "pint.irq0" = "button-irq" }
# ...

[tasks.button.config]
pins = [
    { name = "BUTTON",    pin = { port = 1, pin = 9}, alt = 0, pint = 0, direction = "input", opendrain = "normal" },
    { name = "RED_LED",   pin = { port = 1, pin = 6}, alt = 0, direction = "output", value = true },
    { name = "GREEN_LED", pin = { port = 1, pin = 7}, alt = 0, direction = "output", value = true },
    { name = "BLUE_LED",  pin = { port = 1, pin = 4}, alt = 0, direction = "output", value = true },
]
```

A notification bit corresponding to the above "button-irq" will be
generated and called `notification::BUTTON_IRQ_MASK`.

The task's `build.rs` generates GPIO support code:
```rust
    let task_config = build_util::task_config::<TaskConfig>()?;
    build_lpc55pins::codegen(task_config.pins)`
```

The `port`, `pin`, `alt`, `mode`, `slew`, `invert`, `digimode`, and
`opendrain` tags all represent values found in UM111126.

## Named pins

The `name` field is optional. Giving pins symbolic names in the `app.toml`
can make a task a bit more portable.  Naming a pin will generate a
`pub const NAME: Pin` definition that can be used with the `lpc55-pins`
API instead referring to it as `Pin::PIOx_y`.

Naming a pin also generates a separate `setup_pinname` function that
is called by `setup_pins` unless `setup = false` is part of the pin
configuration.

Using `setup = false` and `name` allows there to be multiple
configurations for the same physical pin that can be used at different
times.

As an example, the action taken on detection of a change on an input
signal can be to change that pin to an output and drive the pin. When
the signal has been handled, the pin can be reverted to an input until
the next event.

```toml
[tasks.example.config]
pins = [
    { name = "SP_RESET_IN",  pin = { port = 0, pin = 13 }, alt = 0, direction = "input", pint = 0 },
    { name = "SP_RESET_OUT", pin = { port = 0, pin = 13 }, alt = 0, direction = "output", setup = false },
]
```

In the above case, `setup_pins()` will call `setup_reset_in()`
but not `setup_reset_out()`. The notification handler for
`notification::SP_RESET_IRQ_MASK` will call `setup_reset_out()` and then
`setup_reset_in()` before returning.
