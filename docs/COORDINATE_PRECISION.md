# Coordinate precision

Issue #18 requires PCB IPC coordinates to round to the nearest nanometre and
generated KiCad coordinate text to use at most six decimal places in millimetres.
The same rule applies to reported coordinates. It does not round prices, timing,
ratios, or strings supplied by the user.

## Implementation contract

`konnect_sexp::geometry::fmt_mm` owns coordinate text. It trims trailing zeros
and writes zero without a minus sign. Existing schematic-editor formatting
delegates to this function.

IPC conversion validates finite values and the signed 64-bit nanometre range
after rounding. Numeric message builders return errors before any mutation.
Computed pad, field, drawing, and circle positions receive the same validation
as direct input. Invalid computed values must not become saturated coordinates.

Rust callers of `mm_to_nm`, `vec2`, `distance`, and the numeric IPC message
builders now handle `Result`. Workspace callers propagate the error before
creating or updating items. Keeping their old infallible signatures would
require either a panic or an invented coordinate for invalid values.

Integer footprint transforms also use checked addition and subtraction.
Rotation checks the computed nanometre values before converting them to integers.

Generated schematic primitives, board graphics, library geometry, and the
schematic editor share `fmt_mm`. JSON writers round named coordinate expressions,
and typed IPC coordinate fields use `serialize_mm`. Neither path walks arbitrary
JSON numbers or edits quoted user text.

The regression suite covers positive and negative decimal grids, half-nanometre
ties, negative zero, invalid values, generated schematic primitives, label
offsets, and schematic and board text. Tests inspect the saved coordinates or
decoded IPC payload rather than echoing the input.

The served regression uses the existing KiCad 10-resaved schematic and board
fixtures. It moves a label's anchor to `167.64` before applying the tested
`-5.08` offset. The output scan checks numeric coordinate atoms and ignores
quoted text that happens to contain a long decimal.

## Verification record

- The decimal-grid regression failed before the fix. At `-16.74 mm`, the
  original conversion produced `-16739999 nm` instead of `-16740000 nm`.
- The generated schematic-primitives regression also failed before the fix
  because the output contained binary floating-point noise.
- Extracted current-source tests passed for the decimal grid, conversion bounds
  and ties, and text formatting. These runs compile the actual helper functions
  and tests with the pinned compiler. They do not prove workspace integration.
- Extracted negative controls failed as expected when the conversion range guard
  was disabled and when text formatting bypassed zero normalization.
- Workspace compilation, the served and mock regressions, the four Rust quality
  gates, and line and branch coverage are pending at this checkpoint. The first
  IPC check found one missed fallible-transform call, which has been corrected
  but is not yet confirmed by a subsequent workspace check.

Real KiCad IPC and CLI verification requires an installed KiCad and remains
unverified. Mock payload tests do not establish real KiCad readback.

## Review corrections

The independent review identified pixel and JSON-value expressions incorrectly
passed to the millimetre rounder. Pixel bounding boxes remain integers. Board
extents round millimetre values before JSON conversion and retain null when the
bounds were not measured. Zone clearance and minimum width now use the shared
formatter in both named and unnamed zones.

Bulk symbol movement validates the computed anchor before and after grid
snapping. Translated property anchors receive the same check. Closed-board
placement validates requested and saved coordinates plus child rotation sums
before constructing a replacement. Text thickness and formatted rotations also
refuse values that cannot be represented safely.

Exact-source isolated regressions reproduced the zone formatter and translated
property boundary failures before these corrections and pass after them. They
are supporting evidence only. Full direct-handler and served no-write
regressions have been added and remain queued with the workspace checks.
