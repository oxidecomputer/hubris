# Oxide Root of Trust (RoT) firmware

As of the writing of this document, the software that runs on the RoT is
identical across all main-line hardware (Gimlet, PSC, Sidecar).

We build the RoT firmware in `oxide-rot-1/app.toml`

`oxide-rot-1` is designed to run on the fixed LPC55S69 chips.
`humility probe` will show a ROM Patch version of `0x8`

```
humility:         chip => LPC55, ROM revision 1, device revision 0x1 (1B), ROM patch 0x8
```

*The expectation is that all new boards going forward will have the fixed
LPC55S69 and that `oxide-rot-1` should be used unless otherwise specified*

The following setups are no longer supported by RoT images:

- All Gimlet Rev A
- All Gimlet Rev B
- All PSC Rev A

Flashing a `oxide-rot-1` setup on an older RoT will not fully boot into
Hubris and will show errors related to undefined instructions:

```
$ humility probe
...
humility:         chip => LPC55, ROM revision 1, device revision 0x1 (1B), ROM patch 0x4
...
humility:           PC => 0x1dc
humility:          PSR => 0x29000006
humility:          MSP => 0x200003b8
humility:          PSP => 0x0
humility:          SPR => 0x0
humility: Fault detected! Raw CFSR: 0x10000
humility: Usage Fault : Undefined instruction
```

