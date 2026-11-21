# KiCad PIC programmer netlist fixture

The schematic and project files are unmodified copies of the [KiCad PIC programmer demo at 10.0.8](https://gitlab.com/kicad/code/kicad/-/tree/10.0.8/demos/pic_programmer). The KiCad contributors distribute the demo files under [CC BY-SA 4.0](https://gitlab.com/kicad/code/kicad/-/blob/10.0.8/LICENSE.CC-BY-SA-4.0), as recorded in the [KiCad license list](https://gitlab.com/kicad/code/kicad/-/blob/10.0.8/LICENSE.README).

| File | SHA-256 |
| --- | --- |
| `pic_programmer.kicad_pro` | `6030132b7fdb0cf9c00967e173359fb2aeb735b9b6e6ea392fcea84e14c787a3` |
| `pic_programmer.kicad_sch` | `cf92bff8aebe19e45444f13b2ccd1c8ad578df6a226acd5381e2eff95e2fc08f` |
| `pic_sockets.kicad_sch` | `8cbf575b53b205280ca277df8affc6477cf20c5d06c63c267cd6fb19b6d771ae` |

`native-nets.tsv` records net names and sorted component pin memberships from `kicad-cli sch export netlist --format kicadxml` using KiCad 10.0.8 on independent copies. It excludes the native `unconnected-*` records because Konnect reports those pins separately. The offline test loads the checked-in schematic and compares its computed result against that file.
