# Zone nets during PCB updates

`update_pcb_from_schematic` and `update_footprints_from_library` read zones
from the requested live board before preparing a mutation. A copper zone
protects only its own net from pad-net reassignment. A GND pour therefore
does not block a change between unrelated signal nets. Rule areas and graphical zones do not protect any net.
KiCad 10.0.5 serializes graphical zones with `CopperZoneSettings` too.
Their optional or stale net metadata is ignored because they are non-copper.
Graphical zones without settings remain accepted.

The vendored protocol stores a copper zone's net in
`Zone.settings.copper_settings.net`. `Zone` has no direct `net` field.
An explicitly unconnected copper zone has net code zero and an empty name.
That state differs from missing net evidence. Missing, malformed, or
inconsistent zone evidence refuses the update before mutation and identifies
the offending zone when its identifier can be decoded.

## Implementation contract

Issue [#16](https://github.com/takurot/Konnect-xai/issues/16) changes live
zone classification in the two existing snapshots. It keeps the current
target checks, reviewed-plan revisions, transactions, and readback paths.
Saved-board zone parsing and zone geometry are outside this change.

One shared decoder owns the distinction between non-copper zones, copper
zones with a known net, and unavailable evidence. Each consumer retains its
existing routed-net collection. This avoids two interpretations of the same
protobuf without replacing either planner.

The acceptance tests cover a named GND copper zone, unrelated signal nets,
keepout rule areas, explicitly unconnected copper zones, malformed payloads,
missing settings and net evidence, and inconsistent type/settings pairs.
Both snapshot consumers must report the same net coverage. Refusals must
occur before commit or item mutation commands.

## Verification record

The baseline reproduction extracted the unchanged
`snapshot_routed_nets` function into a small Rust executable. Its stub client
returned the checked-in KiCad GND copper-zone capture and three board nets.
The assertion failed with `{GND, SIG_A, SIG_B}` instead of `{GND}`. The
baseline executable did not exercise the protobuf decoder or a live editor.

The added consumer tests use the actual IPC client against an NNG mock.
They cover both snapshots, actual KiCad zone captures, malformed evidence,
and the schematic-sync planner's routed-net refusal. Classifier unit tests
also cover teardrops and explicitly graphical zones.

At the implementation checkpoint, these Cargo tests and the four workspace
quality gates remained pending in the serialized verification queue. Direct
`rustfmt --check` on the changed Rust files and `git diff --check` passed.
Line and branch coverage were not measured. Real KiCad IPC was unavailable.
Mock payloads cannot prove behavior in an unobserved editor.

## Graphical-zone serializer compatibility

The [KiCad 10.0.5 serializer](https://github.com/KiCad/kicad/blob/10.0.5/pcbnew/zone.cpp#L239-L294)
selects `ZT_GRAPHICAL` for non-copper zones and still fills
`copper_settings` for every non-rule-area zone. The shared decoder therefore
accepts that pair without interpreting its net. Unknown types and
inconsistent copper or rule-area settings remain errors.

Regression fixtures derive from the checked-in copper-zone capture by
selecting a graphical type and silkscreen layer. They retain copper settings
with a GND name, missing net, or unused stale net with an invalid copper code.
These are serializer-shaped fixtures, not native graphical-zone captures.
The classifier and both actual IPC snapshots must return no routed net for
each fixture. Existing graphical-without-settings coverage remains.
