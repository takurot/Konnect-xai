//! Reading the `(layers …)` stackup table of a `.kicad_pcb`.
//!
//! Layer entries are the one table in the board file that is *not* keyed by a
//! tag. Everything else is `(tag …)` and can be found with [`SexpNode::find_all`];
//! a layer is `(0 "F.Cu" signal)`, whose head is the ordinal itself. There is no
//! tag to match on, so the entries have to be read by shape: every list child of
//! the `(layers …)` node is a layer.
//!
//! ```
//! use konnect_sexp::{parse_sexp, layers};
//! let board = parse_sexp(r#"(kicad_pcb (layers (0 "F.Cu" signal) (2 "B.Cu" signal) (9 "F.Adhes" user)))"#).unwrap();
//! let stack = layers::layers(&board);
//! assert_eq!(stack.len(), 3);
//! assert_eq!(stack[0].name, "F.Cu");
//! assert_eq!(layers::copper(&stack).len(), 2);
//! ```

use crate::parser::SexpNode;

/// One entry of the board stackup.
#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    /// Ordinal as written in the file. Not an index: KiCad leaves gaps
    /// (`0 F.Cu`, `2 B.Cu`, `9 F.Adhes` …) and inner copper occupies 1..=30.
    pub id: i32,
    /// Canonical name — `F.Cu`, `In1.Cu`, `Edge.Cuts`.
    pub name: String,
    /// `signal`, `power`, `mixed`, `jumper` or `user`.
    pub kind: String,
    /// Optional user-facing rename, e.g. `(0 "F.Cu" signal "Top Layer")`.
    pub user_name: Option<String>,
}

impl Layer {
    /// Copper is decided by the canonical name, not by `kind`. See
    /// [`is_copper_name`].
    pub fn is_copper(&self) -> bool {
        is_copper_name(&self.name)
    }
}

/// Is this canonical layer name a copper layer?
///
/// KiCad marks copper with four different kinds (`signal`, `power`, `mixed`,
/// `jumper`) and a board that uses `power` for a plane would be undercounted
/// by a kind allow-list. The `.Cu` suffix is the invariant — which is why this
/// takes a name: a layer read over KiCad's IPC API has no kind at all.
pub fn is_copper_name(name: &str) -> bool {
    name.ends_with(".Cu")
}

/// Read the stackup from a parsed board. Empty if there is no `(layers …)`.
pub fn layers(board: &SexpNode) -> Vec<Layer> {
    let Some(node) = board.find("layers") else {
        return Vec::new();
    };
    node.children()
        .unwrap_or(&[])
        .iter()
        // Skips the head atom (`layers`), which is a child like any other, and
        // any stray atom: a layer is always a list.
        .filter(|child| child.children().is_some())
        .filter_map(layer_from)
        .collect()
}

/// Copper layers only, in file order — the "how many layers is this board"
/// answer, and what a fab house quotes on.
pub fn copper(stack: &[Layer]) -> Vec<&Layer> {
    stack.iter().filter(|l| l.is_copper()).collect()
}

/// How many copper layers a parsed board declares — the number a fab house
/// quotes on.
///
/// This is the one place that answer is computed, so `get_board_info`,
/// `validate_for_manufacturing` and `estimate_cost` cannot disagree (#461:
/// the manufacturing tools counted the substring `signal)` in the file text,
/// which misses every `power`, `mixed` and `jumper` copper layer and quoted a
/// six-layer board as two-layer). A board with no `(layers …)` table has zero
/// copper layers; callers decide what that means, never this function.
pub fn copper_layer_count(board: &SexpNode) -> usize {
    copper(&layers(board)).len()
}

/// The fixed layer names, i.e. every `BoardLayer` variant that is neither
/// `In<n>.Cu` nor `User.<n>` nor a sentinel.
const FIXED_NAMES: &[&str] = &[
    "F.Cu",
    "B.Cu",
    "B.Adhes",
    "F.Adhes",
    "B.Paste",
    "F.Paste",
    "B.SilkS",
    "F.SilkS",
    "B.Mask",
    "F.Mask",
    "Dwgs.User",
    "Cmts.User",
    "Eco1.User",
    "Eco2.User",
    "Edge.Cuts",
    "Margin",
    "B.CrtYd",
    "F.CrtYd",
    "B.Fab",
    "F.Fab",
    "Rescue",
];

/// Highest `In<n>.Cu` / `User.<n>` KiCad defines (`BL_In30_Cu`, `BL_User_45`).
const MAX_INNER_COPPER: u32 = 30;
const MAX_USER: u32 = 45;

/// Is this a layer name KiCad will accept in a board file?
///
/// KiCad does not take arbitrary layer names: the set is closed, and it is the
/// `BoardLayer` enum of the official API protos
/// (`konnect-ipc/proto/board/board_types.proto`), whose variant names map onto
/// file names by dropping the `BL_` prefix and turning the remaining `_` into a
/// `.` — `BL_F_Cu` → `F.Cu`, `BL_User_1` → `User.1`.
///
/// A board carrying a name outside that set is **rejected outright** by KiCad —
/// it does not degrade, the file simply will not open. `layers_canonical_names`
/// in konnect-core keeps this in step with the enum.
pub fn is_canonical_name(name: &str) -> bool {
    if FIXED_NAMES.contains(&name) {
        return true;
    }
    if let Some(n) = name.strip_prefix("In").and_then(|s| s.strip_suffix(".Cu")) {
        return matches!(n.parse::<u32>(), Ok(n) if (1..=MAX_INNER_COPPER).contains(&n))
            && !n.starts_with('0');
    }
    if let Some(n) = name.strip_prefix("User.") {
        return matches!(n.parse::<u32>(), Ok(n) if (1..=MAX_USER).contains(&n))
            && !n.starts_with('0');
    }
    false
}

/// Which ordinal scheme a board file numbers its layers with.
///
/// KiCad 9 renumbered every layer (`B.Cu` 31 → 2, `In1.Cu` 1 → 4, `F.Mask`
/// 39 → 1). KiCad reads a layer by its name and rewrites the ordinal on save,
/// so a writer has to number a new entry in the scheme the file already uses,
/// or its ordinal collides with one the file gives to another layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerNumbering {
    /// KiCad 8 and earlier: `In<n>.Cu` is `n`, `B.Cu` is 31.
    Legacy,
    /// KiCad 9 and later: `In<n>.Cu` is `2n + 2`, `B.Cu` is 2.
    KiCad9,
}

/// The scheme a stackup is numbered in, read from its `F.Cu` and `B.Cu`
/// entries. `None` when either is missing or numbered in neither scheme.
pub fn numbering(stack: &[Layer]) -> Option<LayerNumbering> {
    let id_of = |name: &str| stack.iter().find(|l| l.name == name).map(|l| l.id);
    match (id_of("F.Cu")?, id_of("B.Cu")?) {
        (0, 31) => Some(LayerNumbering::Legacy),
        (0, 2) => Some(LayerNumbering::KiCad9),
        _ => None,
    }
}

/// The fixed layers in KiCad 9's ordinal order after the copper pair: `F.Mask`
/// is 1, `B.Mask` 3, … `Rescue` 37 (`layer_ids.h`, `PCB_LAYER_ID`).
const KICAD9_ODD_IDS: &[&str] = &[
    "F.Mask",
    "B.Mask",
    "F.SilkS",
    "B.SilkS",
    "F.Adhes",
    "B.Adhes",
    "F.Paste",
    "B.Paste",
    "Dwgs.User",
    "Cmts.User",
    "Eco1.User",
    "Eco2.User",
    "Edge.Cuts",
    "Margin",
    "B.CrtYd",
    "F.CrtYd",
    "B.Fab",
    "F.Fab",
    "Rescue",
];

/// The fixed layers in KiCad 8's ordinal order from 31 (`B.Cu`) to 49 (`F.Fab`).
const LEGACY_FROM_31: &[&str] = &[
    "B.Cu",
    "B.Adhes",
    "F.Adhes",
    "B.Paste",
    "F.Paste",
    "B.SilkS",
    "F.SilkS",
    "B.Mask",
    "F.Mask",
    "Dwgs.User",
    "Cmts.User",
    "Eco1.User",
    "Eco2.User",
    "Edge.Cuts",
    "Margin",
    "B.CrtYd",
    "F.CrtYd",
    "B.Fab",
    "F.Fab",
];

/// KiCad 8 defined only `User.1`..`User.9`.
const LEGACY_MAX_USER: u32 = 9;

/// The ordinal KiCad gives a canonical layer name under `numbering`, or `None`
/// for a name that scheme does not define.
///
/// ```
/// use konnect_sexp::layers::{canonical_id, LayerNumbering::*};
/// assert_eq!(canonical_id("In2.Cu", KiCad9), Some(6));
/// assert_eq!(canonical_id("In2.Cu", Legacy), Some(2));
/// assert_eq!(canonical_id("User.1", KiCad9), Some(39));
/// assert_eq!(canonical_id("User.10", Legacy), None);
/// ```
pub fn canonical_id(name: &str, numbering: LayerNumbering) -> Option<i32> {
    if !is_canonical_name(name) {
        return None;
    }
    let inner = inner_copper_index(name);
    let user = name
        .strip_prefix("User.")
        .and_then(|n| n.parse::<u32>().ok());
    let position = |table: &[&str]| table.iter().position(|t| *t == name);
    let id = match numbering {
        LayerNumbering::KiCad9 => match name {
            "F.Cu" => 0,
            "B.Cu" => 2,
            _ => match (inner, user) {
                (Some(n), _) => 2 * n + 2,
                (_, Some(n)) => 37 + 2 * n,
                _ => 2 * position(KICAD9_ODD_IDS)? as u32 + 1,
            },
        },
        LayerNumbering::Legacy => match (name, inner, user) {
            ("F.Cu", _, _) => 0,
            ("Rescue", _, _) => 59,
            (_, Some(n), _) => n,
            (_, _, Some(n)) if n <= LEGACY_MAX_USER => 49 + n,
            (_, _, Some(_)) => return None,
            _ => 31 + position(LEGACY_FROM_31)? as u32,
        },
    };
    i32::try_from(id).ok()
}

/// `n` for `In<n>.Cu`, `None` for every other name.
pub fn inner_copper_index(name: &str) -> Option<u32> {
    is_canonical_name(name)
        .then(|| name.strip_prefix("In")?.strip_suffix(".Cu")?.parse().ok())
        .flatten()
}

fn layer_from(node: &SexpNode) -> Option<Layer> {
    // `(0 "F.Cu" signal "Top Layer")` — the ordinal is child 0, being the head
    // of the list, so every field sits one place earlier than in a tagged node.
    let id = node.get_f64(0)? as i32;
    let name = node.get(1)?.as_str()?.to_string();
    let kind = node
        .get(2)
        .and_then(|n| n.as_str())
        .unwrap_or("user")
        .to_string();
    let user_name = node.get(3).and_then(|n| n.as_str()).map(str::to_string);
    Some(Layer {
        id,
        name,
        kind,
        user_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_sexp;

    fn board(inner: &str) -> SexpNode {
        parse_sexp(&format!("(kicad_pcb (layers {inner}))")).unwrap()
    }

    #[test]
    fn reads_id_name_and_kind() {
        let stack = layers(&board(r#"(0 "F.Cu" signal)"#));
        assert_eq!(
            stack,
            vec![Layer {
                id: 0,
                name: "F.Cu".into(),
                kind: "signal".into(),
                user_name: None
            }]
        );
    }

    #[test]
    fn reads_the_optional_user_name() {
        let stack = layers(&board(r#"(0 "F.Cu" signal "Top Layer")"#));
        assert_eq!(stack[0].user_name.as_deref(), Some("Top Layer"));
    }

    #[test]
    fn the_head_atom_is_not_a_layer() {
        // `(layers …)` yields `layers` itself as a child; counting children
        // blindly overcounts by exactly one.
        let stack = layers(&board(r#"(0 "F.Cu" signal) (2 "B.Cu" signal)"#));
        assert_eq!(stack.len(), 2);
    }

    #[test]
    fn ids_are_read_as_written_not_as_positions() {
        // KiCad leaves gaps; a positional read would report 0,1,2.
        let stack = layers(&board(
            r#"(0 "F.Cu" signal) (2 "B.Cu" signal) (9 "F.Adhes" user)"#,
        ));
        assert_eq!(
            stack.iter().map(|l| l.id).collect::<Vec<_>>(),
            vec![0, 2, 9]
        );
    }

    #[test]
    fn copper_is_by_name_not_by_kind() {
        // `power` and `mixed` are copper too; `user` never is, even on a layer
        // whose name merely contains "Cu".
        let stack = layers(&board(
            r#"(0 "F.Cu" signal) (1 "In1.Cu" power) (2 "In2.Cu" mixed) (3 "B.Cu" jumper) (9 "F.Adhes" user) (60 "Cu.Marks" user)"#,
        ));
        assert_eq!(
            copper(&stack).iter().map(|l| &l.name).collect::<Vec<_>>(),
            vec!["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"]
        );
    }

    #[test]
    fn a_board_without_a_layers_block_is_empty_not_a_panic() {
        assert!(layers(&parse_sexp("(kicad_pcb)").unwrap()).is_empty());
    }

    /// #461: the count a fab quotes on. Every copper kind counts, the user
    /// layers never do, and a board with no table is zero, not two.
    #[test]
    fn copper_layer_count_counts_every_copper_kind() {
        assert_eq!(
            copper_layer_count(&board(
                r#"(0 "F.Cu" signal) (4 "In1.Cu" power) (6 "In2.Cu" signal) (8 "In3.Cu" mixed) (10 "In4.Cu" jumper) (2 "B.Cu" signal) (9 "F.Adhes" user) (25 "Edge.Cuts" user)"#,
            )),
            6
        );
        assert_eq!(
            copper_layer_count(&board(
                r#"(0 "F.Cu" signal "Top Layer") (2 "B.Cu" signal "Bottom")"#
            )),
            2
        );
        assert_eq!(copper_layer_count(&parse_sexp("(kicad_pcb)").unwrap()), 0);
    }

    #[test]
    fn a_malformed_entry_is_skipped_and_the_rest_survive() {
        let stack = layers(&board(r#"(0 "F.Cu" signal) (nonsense) (2 "B.Cu" signal)"#));
        assert_eq!(
            stack.iter().map(|l| &l.name).collect::<Vec<_>>(),
            vec!["F.Cu", "B.Cu"]
        );
    }

    #[test]
    fn canonical_names_are_accepted() {
        for name in [
            "F.Cu",
            "B.Cu",
            "In1.Cu",
            "In30.Cu",
            "Edge.Cuts",
            "Margin",
            "Rescue",
            "User.1",
            "User.45",
            "Dwgs.User",
            "F.CrtYd",
        ] {
            assert!(is_canonical_name(name), "{name} should be canonical");
        }
    }

    #[test]
    fn invented_names_are_rejected() {
        // The one that matters: a caller-supplied name KiCad has never heard of
        // produces a board that will not open at all.
        for name in [
            "TestLayer",
            "MyLayer",
            "",
            "f.cu",
            "F_Cu",
            "In0.Cu",
            "User.0",
        ] {
            assert!(!is_canonical_name(name), "{name} should not be canonical");
        }
    }

    #[test]
    fn out_of_range_and_padded_ordinals_are_rejected() {
        // KiCad stops at In30.Cu / User.45, and does not zero-pad.
        for name in ["In31.Cu", "User.46", "In01.Cu", "User.01", "In.Cu", "User."] {
            assert!(!is_canonical_name(name), "{name} should not be canonical");
        }
    }

    /// KiCad 10's own layer table, as `demos/jetson-agx-thor-baseboard`
    /// writes it, plus the `User.45` end of the range.
    const KICAD10_TABLE: &[(i32, &str)] = &[
        (0, "F.Cu"),
        (4, "In1.Cu"),
        (6, "In2.Cu"),
        (18, "In8.Cu"),
        (62, "In30.Cu"),
        (2, "B.Cu"),
        (9, "F.Adhes"),
        (11, "B.Adhes"),
        (13, "F.Paste"),
        (15, "B.Paste"),
        (5, "F.SilkS"),
        (7, "B.SilkS"),
        (1, "F.Mask"),
        (3, "B.Mask"),
        (17, "Dwgs.User"),
        (19, "Cmts.User"),
        (21, "Eco1.User"),
        (23, "Eco2.User"),
        (25, "Edge.Cuts"),
        (27, "Margin"),
        (31, "F.CrtYd"),
        (29, "B.CrtYd"),
        (35, "F.Fab"),
        (33, "B.Fab"),
        (37, "Rescue"),
        (39, "User.1"),
        (55, "User.9"),
        (127, "User.45"),
    ];

    /// KiCad 8's table (`ecc83` before KiCad 9 renumbered it).
    const LEGACY_TABLE: &[(i32, &str)] = &[
        (0, "F.Cu"),
        (1, "In1.Cu"),
        (30, "In30.Cu"),
        (31, "B.Cu"),
        (32, "B.Adhes"),
        (33, "F.Adhes"),
        (34, "B.Paste"),
        (35, "F.Paste"),
        (36, "B.SilkS"),
        (37, "F.SilkS"),
        (38, "B.Mask"),
        (39, "F.Mask"),
        (40, "Dwgs.User"),
        (41, "Cmts.User"),
        (42, "Eco1.User"),
        (43, "Eco2.User"),
        (44, "Edge.Cuts"),
        (45, "Margin"),
        (46, "B.CrtYd"),
        (47, "F.CrtYd"),
        (48, "B.Fab"),
        (49, "F.Fab"),
        (50, "User.1"),
        (58, "User.9"),
    ];

    #[test]
    fn canonical_ids_match_kicads_own_tables() {
        for (numbering, table) in [
            (LayerNumbering::KiCad9, KICAD10_TABLE),
            (LayerNumbering::Legacy, LEGACY_TABLE),
        ] {
            for (id, name) in table {
                assert_eq!(
                    canonical_id(name, numbering),
                    Some(*id),
                    "{name} {numbering:?}"
                );
            }
        }
    }

    #[test]
    fn names_a_scheme_does_not_define_have_no_id() {
        assert_eq!(canonical_id("User.10", LayerNumbering::Legacy), None);
        assert_eq!(canonical_id("Custom.Cu", LayerNumbering::KiCad9), None);
        assert_eq!(canonical_id("In31.Cu", LayerNumbering::KiCad9), None);
    }

    #[test]
    fn numbering_is_read_from_the_copper_pair() {
        let kicad9 = layers(&board(r#"(0 "F.Cu" signal) (2 "B.Cu" signal)"#));
        let legacy = layers(&board(r#"(0 "F.Cu" signal) (31 "B.Cu" signal)"#));
        let odd = layers(&board(r#"(1 "F.Cu" signal) (2 "B.Cu" signal)"#));
        let no_back = layers(&board(r#"(0 "F.Cu" signal)"#));
        assert_eq!(numbering(&kicad9), Some(LayerNumbering::KiCad9));
        assert_eq!(numbering(&legacy), Some(LayerNumbering::Legacy));
        assert_eq!(numbering(&odd), None);
        assert_eq!(numbering(&no_back), None);
    }

    #[test]
    fn inner_copper_index_reads_only_inner_copper() {
        assert_eq!(inner_copper_index("In7.Cu"), Some(7));
        assert_eq!(inner_copper_index("F.Cu"), None);
        assert_eq!(inner_copper_index("In07.Cu"), None);
        assert_eq!(inner_copper_index("User.7"), None);
    }

    #[test]
    fn tab_indentation_is_irrelevant_to_a_tree_read() {
        // The shape is what matters; KiCad 10 writes tabs, 9 writes spaces.
        let spaces = parse_sexp("(kicad_pcb\n  (layers\n    (0 \"F.Cu\" signal)\n  )\n)").unwrap();
        let tabs = parse_sexp("(kicad_pcb\n\t(layers\n\t\t(0 \"F.Cu\" signal)\n\t)\n)").unwrap();
        assert_eq!(layers(&spaces), layers(&tabs));
    }
}
