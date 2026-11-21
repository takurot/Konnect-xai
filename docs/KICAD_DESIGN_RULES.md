# KiCad fabrication constraints and netclasses

Close any KiCad editor that owns the target project before changing its design
rules. The writers check the exact board when IPC is available, refuse a KiCad
project lock and compare the source revision before replacing the project file.
The PCB itself is not modified.

`get_design_rules` reports values from the saved sibling `.kicad_pro`. A
missing setting is reported as `null` and has the KiCad default on load.
`set_design_rules` validates the entire request before writing, uses KiCad's
nanometre precision and includes the saved values in its response. A request
that repeats stored values leaves the file untouched.

| Tool arguments | KiCad 10 load range, inclusive (mm) |
|---|---:|
| `min_clearance`, `min_trace_width`, `min_via_drill`, `min_via_size`, `min_via_annular_width`, `min_text_thickness` | 0–25 |
| `min_hole_to_hole` | 0–10 |
| `min_hole_clearance`, `min_text_height` | 0–100 |
| `min_silk_clearance` | −10–100 |
| `min_copper_edge_clearance` | −0.01–25 |

The edge clearance limit includes KiCad's legacy `-0.01` setting. The ranges
come from [KiCad 10.0.8's board settings loader](https://github.com/KiCad/kicad-source-mirror/blob/10.0.8/pcbnew/board_design_settings.cpp).
KiCad can otherwise silently reset out-of-range project settings to defaults.
Existing project values are reported as stored. Reopen the project in KiCad
after a successful update to use the new values.

For example, a fabricator's chosen copper-to-edge clearance of 0.3 mm,
minimum via annular width of 0.13 mm and silkscreen height and stroke minima
of 0.8 and 0.15 mm can be set in one `set_design_rules` call. Check the values
for the specific fabrication service used for the board.

`create_netclass` accepts `diff_pair_width`, `diff_pair_gap` and
`diff_pair_via_gap` in mm. Omitting one when updating a class preserves its
current value. Named classes inherit omitted trace widths and gaps from
`Default`. KiCad 10 leaves an omitted via gap unset on named classes.
`get_netclasses` reports the unset value as `null` and identifies the values
that inherit.

`delete_netclass` refuses to remove `Default`. It removes patterns and saved
label assignments to the deleted class and preserves other assignments and
class priorities. A net with surviving matches uses those classes and keeps
KiCad's normal fallback rules. A net with no surviving
matches falls back to `Default` for all properties. The tool backfills missing
fields in an explicit incomplete `Default` so affected nets retain a complete
fallback. This edits project settings only. If a schematic netclass label
still names the deleted class, a later schematic sync may restore that label
assignment.

Generic custom-rule writing requires a reliable KiCad parser result before
installation. On a stock KiCad 10.0.8 fixture, a supported custom clearance
constraint changed the reported violations, while an unrecognized constraint
produced the same ordinary DRC report and exit status as running with no
custom rule. That CLI command cannot validate general rule input. The existing
`set_layer_constraints` tool handles its fixed clearance and trace-width
forms. For a change, pass the exact case-sensitive layer name from the saved
PCB's enabled layer table. Board display aliases and disabled inner layers
are refused before the rules file is opened for writing. An existing rule in
the file is not revalidated by that operation.
