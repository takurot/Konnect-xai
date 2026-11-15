# Stock placement fidelity fixtures

These two unmodified KiCad community library footprints come from
[kicad/libraries/kicad-footprints](https://gitlab.com/kicad/libraries/kicad-footprints),
tag `10.0.8`, commit `a337146e090931ad1176e20ed0b7100990fac5fa`:

- [RF_Module.pretty/ESP32-WROOM-32.kicad_mod](https://gitlab.com/kicad/libraries/kicad-footprints/-/blob/a337146e090931ad1176e20ed0b7100990fac5fa/RF_Module.pretty/ESP32-WROOM-32.kicad_mod)
  contains a keepout zone and per-pad heatsink/zone-connection clauses.
- [NetTie.pretty/NetTie-2_SMD_Pad0.5mm.kicad_mod](https://gitlab.com/kicad/libraries/kicad-footprints/-/blob/a337146e090931ad1176e20ed0b7100990fac5fa/NetTie.pretty/NetTie-2_SMD_Pad0.5mm.kicad_mod)
  contains `net_tie_pad_groups`.

The fixtures retain the KiCad community's CC-BY-SA-4.0 library license and
design exception in [LICENSE.md](LICENSE.md), copied from the same commit.
They are test data with their own license, separate from this repository's
source-code license.

`pcb_components` tests exercise live refusal with a named reference, library
identifier and unsupported clause, zero IPC creation, and an unchanged board
file. The closed-file placement test verifies the same clauses survive;
ESP32's keepout polygon is checked in the saved board. ESP32 is refused at its
first unsupported pad property before the later keepout is encountered.

SHA-256 of the unchanged upstream files:

```text
af11e3ded30556624b02dbdb6b72e7ee6ec829fa0c549365b154eb273f7dbf7c  ESP32-WROOM-32.kicad_mod
6fa0592afb0af821b98f26636b74e8c3eb93c1852bf6bd3872edbbe20f67d3e0  NetTie-2_SMD_Pad0.5mm.kicad_mod
```
