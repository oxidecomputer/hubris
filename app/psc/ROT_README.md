# Where is the RoT build?

As of the writing of this document, the software that runs on the RoT is
identical across all different components (gimlet, psc, sidecar) for
a particular variant (rev-a vs rev-b vs rev-c). We currently only build
the RoT in `gimlet-rot`. Those builds can be flashed on the RoT on
all sleds

# Which RoT build do I use?

tl;dr You probably want `gimlet-rot-c` unless you know that a board is using an
older RoT

More detailed information:

`gimlet-rot-b` is designed to run on the older LPC55S28 chips.
`humility probe` will show a ROM Patch version of `0x4`

```
humility:         chip => LPC55, ROM revision 1, device revision 0x1 (1B), ROM patch 0x4
```

`gimlet-rot-c` is designed to run on the fixed LPC55S69 chips.
`humility probe` will show a ROM Patch version of `0x8`

```
humility:         chip => LPC55, ROM revision 1, device revision 0x1 (1B), ROM patch 0x8
```

*The expectation is that all new boards going forward will have the fixed
LPC55S69 and that `gimlet-rot-c` should be used unless otherwise specified*

The following setups should use `gimlet-rot-b`:

- All Gimlet Rev A
- All Gimlet Rev B
- All PSC Rev A

Flashing a `gimlet-rot-c` setup on a not-fixed RoT will not fully boot into
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

Flashing a `gimlet-rot-b` setup on a fixed RoT will boot although this setup is
not recommended as the LPC55S69 and LPC55S28 have different amounts of flash.
