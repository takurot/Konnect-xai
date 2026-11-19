# Schematic overlap fixture

KiCad 10.0.8 force-resaved this sheet with stock `Device:R` and `power:GND`
graphics from the existing native single-pin-net fixture. The rotated and
mirrored resistor is horizontal. Its pin at (96.19, 100) meets the GND tip.
The resistor body starts at x = 97.46, which is also the outer edge of the
power symbol's graphic envelope. Their full symbol envelopes intersect
because the resistor's pin extends beyond its body.

The stock symbol and wire geometry was checked with `kicad-cli sch export svg`.
The visible R1 field extents are horizontal around (100, 95). The global
label at (140, 130) extends left and the hierarchical label at (175, 130)
extends up. Both wires depart their label's anchor in the opposite direction.
The two local labels are left-justified, separated by 3 mm and visibly overlap.
