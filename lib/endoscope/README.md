# Endoscope for the STM32H753

This is a small RAM-resident program that computes the Sha3-256 digest
of the STM32H753 active flash bank. It is run under the control of the
STM32's debug port which in turn is controlled by a debugger or the Root
of Trust (LPC55S69) in an Oxide Computer system.

The program's results are placed in a known memory location
which is read out when the program terminates through execution of
a breakpoint instruction.

The program can be tested by itself using gdb/openocd or probe-rs via
an ST-LINK.

## RoT Execution method

Two symbols are needed from the program's symbol table:
  - `__vector_table: [u32]` contains the initial_sp at [0] and initial_pc
    at [1] and is also the load address of the program.
  - "SHARED": The address of the shared data structure.

The steps for execution are along these lines:

  - (RoT) Detect an SP RESET signal
  - Assert the SP RESET signal to keep the SP
  - Initialize the SWD interface
  - Set VC_CORERESET to trap on the STM32 reset condition.
  - De-assert the SP RESET signal
  - ensure that the STM32 halted due to VC_CORERESET
  - Inject the program into the STM32's RAM at the address `__vector_table`.
  - Set The STM32 core register PC/DR to the value in `__vector_table[1]`.
  - Set The STM32 core register MSP to the value in `__vector_table[0]`.
  - Set The STM32 VTOR to the value of the symbol `__vector_table`.
  - Continue execution
  - Poll DHCSR for S_HALT state or timeout.
  - The attestation log will be reset.
  - If halted && `SHARED.magic == Shared::MAGIC` && `SHARED.state == State::Done`
      - then read out the `digest` field.
  - Clean up any STM32 debug state.
  - Tear down the SWD session.
  - Assert and de-assert the SP RESET signal to boot the SP from FLASH.

On failure, including the presence of an active ST-LINK dongle, the RoT will
invalidate any previous measurements that have been recorded.

## Size and Performance

```bash
# The program is about 5KiB and takes less than 0.5 seconds to inject and run.
$ find target/thumbv8m.main-none-eabihf/release/build \
  -name endoscope.bin -print -exec stat -c '%s' '{}' ';'
target/thumbv8m.main-none-eabihf/release/build/drv-lpc55-swd-da6462ef675419cb/out/endoscope.bin
5740
```

Building as a cargo `bindeps` artifact allows the source to be maintained
in the Hubris repo and removes any concern that it is out of date with
respect to the RoT firmware.

However, as a `bindeps` artifact, the profile that it is built with is not
allowed to specify `lto` or `panic`.

Not being able to use the desired profile costs something around an extra 462 bytes at
the time of writing.

Rust "code golf" opportunities for space and time include:
  - Fix the build profile `lto` and `panic` prohibition described above,
  - Use an FFI SHA3 library, if a more compact or faster implementation can be found.
  - On the RoT side, inject the code more efficiently.

```bash
# Building it as a stand-alone bin results in a smaller executable (4660 bytes)
$ arm-none-eabi-size target/thumbv7em-none-eabihf/release/endoscope
   text	   data	    bss	    dec	    hex	filename
   4608	     48	      4	   4660	   1234	target/thumbv7em-none-eabihf/release/endoscope
```

## Testing

The program can be tested in isolation using `gdb`. But, it is simpler to
use the probe-rs-tools.

### Setup

In this case, the probe is an ST-LINK attached to an STM32H753xi.
Since there is only one ST-LINK on this system, the probe's serial number does
not need to be specified.

The smaller isolated build is useful for development:

```sh
$ cargo build --release --manifest-path lib/endoscope/Cargo.toml --target thumbv7em-none-eabihf --bin endoscope --features soc_stm32h753
$ ELF=${PWD}/target/thumbv7em-none-eabihf/release/endoscope
$ size $ELF
   text    data     bss     dec     hex filename
   4424      48       4    4476    117c ${PWD}/target/thumbv7em-none-eabihf/release/endoscope
```

The blob produced during the build of the RoT swd task is what will be used
in production:

```sh
$ cargo clean # So that there is only one version available.
$ cargo xtask build $APP swd
$ ELF=$(ls -d ${PWD}/target/thumbv7em-none-eabihf/release/deps/artifact/endoscope-*/bin/endoscope-????????????????)
$ size $ELF
   text    data     bss     dec     hex filename
   4920      48       4    4972    136c ${PWD}/target/thumbv7em-none-eabihf/release/deps/artifact/endoscope-05d6ebcdad662d34/bin/endoscope-05d6ebcdad662d34
```

```sh
PROBE="0483:3754" # 0483:3754:${ST_LINK_SERIALNO} to disambiguate
CHIP=stm32h753xi
# See above to set the environment variable "ELF"
cargo install probe-rs-tools
```

### Get symbol values from the ELF file

```sh
VTABLE=0x$(arm-none-eabi-nm -C "${ELF}" | awk '/__vector_table/ {print $1}' -)
SHARED=0x$(arm-none-eabi-nm -C "${ELF}" | awk '/SHARED/ {print $1}' -)
```

### Running

```sh
$ time probe-rs run --probe ${PROBE} --chip ${CHIP} "${ELF}"
     Finished in 0.02s
Frame 0: breakpoint @ 0x24000356
       ${REPO}/src/main.rs:69:13
Frame 1: DefaultHandler @ 0x2400034a
       ${REPO}/src/main.rs:59:1
Error: CPU halted unexpectedly.

real    0m5.677s
user    0m0.017s
sys     0m0.122s
```

The unexpected halt mentioned above is actually expected.

That run was using the STM32H753's SRAM with the default clocking after reset.

It takes about 0.5 seconds with the correct clocks set and using
the ITCM/DTCM memories.

### Read the Results

The results are in a `struct Shared`.
Given that `endoscope` and the `swd` task are compiled together, no structure magic
number or versioning is required and compile- and link-time constants are trustworthy.

```rust
#[repr(u32)]
pub enum State {
    #[allow(dead_code)]
    Preboot = 0,
    Running = 0x1de6060,
    Done = 0x1dec1a0,
}

#[repr(C)]
pub struct Shared {
    pub state: State,
    pub digest: [u8; DIGEST_SIZE],
}

impl Shared {
    const MAGIC: u32 = 0x1de2019;
}
```

We'll dump the four 32-bit words and then the 32-byte SHA3-256 digest.

```sh
$ probe-rs read --probe ${PROBE} --chip ${CHIP} b32 ${SHARED} 4
01de2019 01dec1a0 08000000 00100000 
# That is the magic = 0x1de_2019, state: State = 0x1de_c1a0 /* Done */
# Flash start address 0x0800_0000, and Flash length 0x0010_0000

$ probe-rs read --probe ${PROBE} --chip ${CHIP} b8 $(( ${SHARED} + 16 )) 32 |
  sed -e 's/ //g'
036f35ccafba2a6e09d755db33a5be73dbc2eed0335eb21e3b78be4e15b8f754
# That is the Sha3-256 digest of the entire active flash bank.
```

Or, as the struct being used:

```rust
Shared {
    state: State::Done,
    digest: [
        03, 6f, 35, cc, af, ba, 2a, 6e,
        09, d7, 55, db, 33, a5, be, 73,
        db, c2, ee, d0, 33, 5e, b2, 1e,
        3b, 78, be, 4e, 15, b8, f7, 54, 
    ],
}
```
