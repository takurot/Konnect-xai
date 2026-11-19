# Rotated stock SOIC-8 Specctra fixture

Generated with KiCad 10.0.8 using the stock
`Package_SO:SOIC-8_3.9x4.9mm_P1.27mm` footprint. Two front-side footprints
share eight separate nets by pad number. U1 is placed at 90° and U2 at 0°.
The board has no pre-existing routing and the native export was produced by
KiCad's `ExportSpecctraDSN`. Native outlines include a courtyard polygon, and
the padstack uses fractional-micrometre polygon vertices.

The `specctra_stock_soic8_bottom_4layer` fixture starts with the same board.
With KiCad 10.0.8 it adds two inner copper layers, flips U1 to the back
using `FOOTPRINT.Flip(U1.GetPosition(), True)`, and changes its first pad
to absolute rotation −75°. An unnumbered, netless 0.8 mm NPTH is added
to U1 at a board offset of +4 mm X and +2 mm Y. KiCad's exporter represents
the hole as four circular keepouts and writes the pin with relative
rotation 345°.
