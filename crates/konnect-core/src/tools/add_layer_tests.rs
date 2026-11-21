//! `add_layer` against KiCad's layer rules (#127), each measured with
//! `kicad-cli pcb upgrade` 10.0.8: KiCad refuses a board with an odd copper
//! count, drops inner copper listed after `B.Cu`, collapses a repeated name, and
//! numbers layers in the scheme the file was written in.

use super::*;
use crate::tools::pcb_board::board_mock::ctx_talking_to;

/// `create_project`'s board: KiCad 10 ordinals, in KiCad's file order.
const KICAD10_BOARD: &str = "(kicad_pcb\n\t(version 20260206)\n\t(layers\n\t\t(0 \"F.Cu\" signal)\n\t\t(2 \"B.Cu\" signal)\n\t\t(9 \"F.Adhes\" user \"F.Adhesive\")\n\t\t(1 \"F.Mask\" user)\n\t\t(25 \"Edge.Cuts\" user)\n\t)\n\t(setup\n\t\t(pad_to_mask_clearance 0.05)\n\t)\n)\n";

const LEGACY_FOUR_LAYER: &str = "(kicad_pcb\n  (version 20221018)\n  (layers\n    (0 \"F.Cu\" signal)\n    (1 \"In1.Cu\" signal)\n    (2 \"In2.Cu\" signal)\n    (31 \"B.Cu\" signal)\n    (39 \"F.Mask\" user)\n    (44 \"Edge.Cuts\" user)\n  )\n)\n";

fn board_with(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("board.kicad_pcb");
    std::fs::write(&path, content).unwrap();
    (dir, path)
}

async fn add(path: &Path, args: serde_json::Value) -> CallToolResult {
    let mut call = args;
    call["board"] = json!(path);
    handle_add_layer(&call, &ctx_talking_to(String::new()))
        .await
        .unwrap()
}

fn body(result: &CallToolResult) -> serde_json::Value {
    let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
        panic!("text response required");
    };
    serde_json::from_str(text).unwrap_or_else(|_| json!(text))
}

fn table(path: &Path) -> Vec<(i32, String, String)> {
    let tree = parse_sexp(&std::fs::read_to_string(path).unwrap()).unwrap();
    konnect_sexp::layers::layers(&tree)
        .into_iter()
        .map(|l| (l.id, l.name, l.kind))
        .collect()
}

fn entry(id: i32, name: &str, kind: &str) -> (i32, String, String) {
    (id, name.to_string(), kind.to_string())
}

#[tokio::test]
async fn a_two_layer_board_becomes_four_layers_in_kicads_order() {
    let (_dir, path) = board_with(KICAD10_BOARD);
    let result = add(&path, json!({"layer_name": "In1.Cu"})).await;
    assert!(!result.is_error, "{}", body(&result));
    assert_eq!(
        table(&path),
        vec![
            entry(0, "F.Cu", "signal"),
            entry(4, "In1.Cu", "signal"),
            entry(6, "In2.Cu", "signal"),
            entry(2, "B.Cu", "signal"),
            entry(9, "F.Adhes", "user"),
            entry(1, "F.Mask", "user"),
            entry(25, "Edge.Cuts", "user"),
        ]
    );
    let answer = body(&result);
    assert_eq!(answer["added_layer"], "In1.Cu");
    assert_eq!(answer["id"], 4);
    assert_eq!(answer["copper_layer_count"], 4);
    assert_eq!(
        answer["added_layers"],
        json!([
            {"name": "In1.Cu", "id": 4, "type": "signal"},
            {"name": "In2.Cu", "id": 6, "type": "signal"},
        ])
    );
}

#[tokio::test]
async fn asking_for_the_second_of_a_pair_adds_the_same_pair_typing_only_the_named_one() {
    let (_dir, path) = board_with(KICAD10_BOARD);
    let result = add(
        &path,
        json!({"layer_name": "In2.Cu", "layer_type": "power"}),
    )
    .await;
    assert!(!result.is_error, "{}", body(&result));
    assert_eq!(body(&result)["id"], 6);
    let copper: Vec<_> = table(&path).into_iter().take(4).collect();
    assert_eq!(
        copper,
        vec![
            entry(0, "F.Cu", "signal"),
            entry(4, "In1.Cu", "signal"),
            entry(6, "In2.Cu", "power"),
            entry(2, "B.Cu", "signal"),
        ]
    );
}

#[tokio::test]
async fn a_layer_already_on_the_board_is_refused_without_writing() {
    let (_dir, path) = board_with(KICAD10_BOARD);
    assert!(!add(&path, json!({"layer_name": "In2.Cu"})).await.is_error);
    let before = std::fs::read_to_string(&path).unwrap();
    for name in ["In2.Cu", "In1.Cu", "F.Mask"] {
        let result = add(&path, json!({"layer_name": name})).await;
        assert!(result.is_error, "{name} added twice");
        assert!(body(&result).to_string().contains("already"), "{name}");
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[tokio::test]
async fn a_legacy_board_is_numbered_in_its_own_scheme() {
    let (_dir, path) = board_with(LEGACY_FOUR_LAYER);
    assert!(!add(&path, json!({"layer_name": "In3.Cu"})).await.is_error);
    let user = add(&path, json!({"layer_name": "User.1"})).await;
    assert!(!user.is_error, "{}", body(&user));
    assert_eq!(body(&user)["id"], 50);
    assert_eq!(
        table(&path),
        vec![
            entry(0, "F.Cu", "signal"),
            entry(1, "In1.Cu", "signal"),
            entry(2, "In2.Cu", "signal"),
            entry(3, "In3.Cu", "signal"),
            entry(4, "In4.Cu", "signal"),
            entry(31, "B.Cu", "signal"),
            entry(39, "F.Mask", "user"),
            entry(44, "Edge.Cuts", "user"),
            entry(50, "User.1", "user"),
        ]
    );
}

#[tokio::test]
async fn a_user_layer_takes_kicad_10s_id_and_user_type() {
    let (_dir, path) = board_with(KICAD10_BOARD);
    let result = add(&path, json!({"layer_name": "User.1"})).await;
    assert!(!result.is_error, "{}", body(&result));
    assert_eq!(body(&result)["id"], 39);
    assert_eq!(body(&result)["type"], "user");
    assert_eq!(table(&path).last(), Some(&entry(39, "User.1", "user")));
}

#[tokio::test]
async fn a_layer_type_that_does_not_fit_the_layer_is_refused() {
    let (_dir, path) = board_with(KICAD10_BOARD);
    for (name, kind) in [("User.1", "signal"), ("In1.Cu", "user")] {
        let result = add(&path, json!({"layer_name": name, "layer_type": kind})).await;
        assert!(result.is_error, "{name} as {kind}");
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), KICAD10_BOARD);
}

#[tokio::test]
async fn inner_copper_that_cannot_keep_the_stack_whole_is_refused() {
    let stackup = KICAD10_BOARD.replace(
        "\t(setup\n",
        "\t(setup\n\t\t(stackup\n\t\t\t(layer \"F.Cu\" (type \"copper\"))\n\t\t\t(layer \"B.Cu\" (type \"copper\"))\n\t\t)\n",
    );
    let after_technical = KICAD10_BOARD.replace(
        "\t\t(25 \"Edge.Cuts\" user)\n",
        "\t\t(25 \"Edge.Cuts\" user)\n\t\t(4 \"In1.Cu\" signal)\n\t\t(6 \"In2.Cu\" signal)\n",
    );
    let odd = KICAD10_BOARD.replace(
        "\t\t(2 \"B.Cu\" signal)\n",
        "\t\t(4 \"In1.Cu\" signal)\n\t\t(2 \"B.Cu\" signal)\n",
    );
    let misnumbered =
        "(kicad_pcb (layers (0 \"F.Cu\" signal) (2 \"B.Cu\" signal) (44 \"Edge.Cuts\" user)))";
    for (case, content, name) in [
        ("stackup", stackup.as_str(), "In1.Cu"),
        ("past the next pair", KICAD10_BOARD, "In3.Cu"),
        (
            "inner copper after B.Cu",
            after_technical.as_str(),
            "In3.Cu",
        ),
        ("odd copper count", odd.as_str(), "In2.Cu"),
        ("ordinal outside the scheme", misnumbered, "User.1"),
    ] {
        let (_dir, path) = board_with(content);
        let result = add(&path, json!({"layer_name": name})).await;
        assert!(result.is_error, "{case}: {}", body(&result));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), content, "{case}");
    }
}

#[tokio::test]
async fn a_board_with_a_stackup_refuses_a_layer_its_stackup_lists() {
    let stackup = KICAD10_BOARD.replace(
        "\t(setup\n",
        "\t(setup\n\t\t(stackup\n\t\t\t(layer \"F.Cu\" (type \"copper\"))\n\t\t)\n",
    );
    let (_dir, path) = board_with(&stackup);
    for name in ["F.Paste", "B.SilkS", "B.Mask"] {
        let result = add(&path, json!({"layer_name": name})).await;
        assert!(result.is_error, "{name}: {}", body(&result));
        assert!(body(&result).to_string().contains("stackup"), "{name}");
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), stackup);
}

#[tokio::test]
async fn a_space_indented_table_keeps_its_layout() {
    let (_dir, path) = board_with(LEGACY_FOUR_LAYER);
    assert!(!add(&path, json!({"layer_name": "In4.Cu"})).await.is_error);
    assert!(!add(&path, json!({"layer_name": "User.2"})).await.is_error);
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "(kicad_pcb\n  (version 20221018)\n  (layers\n    (0 \"F.Cu\" signal)\n    (1 \"In1.Cu\" signal)\n    (2 \"In2.Cu\" signal)\n    (3 \"In3.Cu\" signal)\n    (4 \"In4.Cu\" signal)\n    (31 \"B.Cu\" signal)\n    (39 \"F.Mask\" user)\n    (44 \"Edge.Cuts\" user)\n    (51 \"User.2\" user)\n  )\n)\n"
    );
}

fn board_with_inner(count: u32) -> String {
    let inner: String = (1..=count)
        .map(|i| format!("\t\t({} \"In{i}.Cu\" signal)\n", 2 * i + 2))
        .collect();
    KICAD10_BOARD.replace("\t\t(2 \"B.Cu\"", &format!("{inner}\t\t(2 \"B.Cu\""))
}

#[tokio::test]
async fn the_last_pair_is_in29_and_in30_and_nothing_follows_it() {
    let (_dir, path) = board_with(&board_with_inner(28));
    let result = add(&path, json!({"layer_name": "In30.Cu"})).await;
    assert!(!result.is_error, "{}", body(&result));
    assert_eq!(
        body(&result)["added_layers"],
        json!([
            {"name": "In29.Cu", "id": 60, "type": "signal"},
            {"name": "In30.Cu", "id": 62, "type": "signal"},
        ])
    );
    assert_eq!(body(&result)["copper_layer_count"], 32);
    let before = std::fs::read_to_string(&path).unwrap();
    assert!(add(&path, json!({"layer_name": "In31.Cu"})).await.is_error);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

#[test]
fn a_table_that_does_not_read_back_as_planned_is_refused() {
    let planned = konnect_sexp::layers::layers(&parse_sexp(KICAD10_BOARD).unwrap());
    let misplaced = KICAD10_BOARD.replace(
        "\t\t(25 \"Edge.Cuts\" user)\n",
        "\t\t(25 \"Edge.Cuts\" user)\n\t\t(4 \"In1.Cu\" signal)\n",
    );
    assert!(layer_table_reads_back(KICAD10_BOARD, &planned).is_ok());
    assert!(layer_table_reads_back(&misplaced, &planned).is_err());
    assert!(layer_table_reads_back("(kicad_pcb (layers", &planned).is_err());
}

#[tokio::test]
async fn a_board_with_a_stackup_still_takes_a_user_layer() {
    let stackup = KICAD10_BOARD.replace(
        "\t(setup\n",
        "\t(setup\n\t\t(stackup\n\t\t\t(layer \"F.Cu\" (type \"copper\"))\n\t\t)\n",
    );
    let (_dir, path) = board_with(&stackup);
    assert!(!add(&path, json!({"layer_name": "User.2"})).await.is_error);
    assert_eq!(table(&path).last(), Some(&entry(41, "User.2", "user")));
}

#[tokio::test]
async fn a_single_line_table_keeps_copper_in_order() {
    let (_dir, path) =
        board_with("(kicad_pcb (layers (0 \"F.Cu\" signal) (2 \"B.Cu\" signal \"Bottom\")))");
    assert!(!add(&path, json!({"layer_name": "In1.Cu"})).await.is_error);
    let names: Vec<_> = table(&path).into_iter().map(|(_, n, _)| n).collect();
    assert_eq!(names, ["F.Cu", "In1.Cu", "In2.Cu", "B.Cu"]);
}
