use super::*;
use std::sync::Arc;

fn context() -> ToolContext {
    ToolContext::new(
        super::super::ServerConfig::default(),
        Arc::new(crate::router::ToolRouter::new()),
    )
}

fn decoded(result: CallToolResult) -> Value {
    assert!(!result.is_error, "{result:?}");
    let crate::mcp::protocol::ToolContent::Text { text } = &result.content[0] else {
        panic!("expected a JSON result");
    };
    serde_json::from_str(text).unwrap()
}

async fn add(parent: &Path, name: &str, file: &str) -> Value {
    decoded(
        handle_add_hierarchical_sheet(
            &json!({"schematic": parent, "sheet_name": name, "sheet_file": file}),
            &context(),
        )
        .await
        .unwrap(),
    )
}

fn assert_instances(file: &Path, expected: &[String]) {
    let mut schematic = cse::Schematic::load(file).unwrap();
    let context = super::super::sheet_instance_context(file, &mut schematic).unwrap();
    assert_eq!(context.instance_paths, expected);
    super::super::validate_sheet_instance_state(file, &schematic, &context).unwrap();
}

fn symbol(file: &Path, reference: &str) {
    let mut schematic = cse::Schematic::load(file).unwrap();
    let mut symbol = cse::Symbol::new("Device:R", 20.0, 30.0);
    symbol.set_reference(reference);
    schematic.add_symbol(symbol);
    schematic.overwrite().unwrap();
}

#[tokio::test]
async fn nested_additions_allocate_across_all_live_placements() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project.kicad_sch");
    create_blank_schematic(&root).unwrap();
    std::fs::write(root.with_extension("kicad_pro"), "{}").unwrap();

    assert_eq!(add(&root, "A", "a.kicad_sch").await["sheet"]["page"], "2");
    assert_eq!(add(&root, "B", "b.kicad_sch").await["sheet"]["page"], "3");
    let a = root.with_file_name("a.kicad_sch");
    assert_eq!(
        add(&a, "Nested", "nested.kicad_sch").await["sheet"]["page"],
        "4"
    );
    let b = root.with_file_name("b.kicad_sch");
    assert_eq!(
        add(&b, "Another", "another.kicad_sch").await["sheet"]["page"],
        "5"
    );
    decoded(
        handle_delete_sheet(
            &json!({"schematic": b, "sheet_name": "Another"}),
            &context(),
        )
        .await
        .unwrap(),
    );
    assert_eq!(add(&root, "C", "c.kicad_sch").await["sheet"]["page"], "6");
}

#[tokio::test]
async fn writes_use_both_configured_root_ids_and_share_page_allocator() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("design-a.kicad_sch");
    let second = directory.path().join("design-b.kicad_sch");
    create_blank_schematic(&first).unwrap();
    create_blank_schematic(&second).unwrap();
    let first_uuid = "4187d28e-1569-4eba-b9b7-e900f7d6d8e3";
    let second_uuid = "ef67c491-50cc-4611-9d4c-f378bfa497c0";
    std::fs::write(
        directory.path().join("controller.kicad_pro"),
        json!({"schematic": {"top_level_sheets": [
            {"filename": "design-a.kicad_sch", "uuid": first_uuid},
            {"filename": "design-b.kicad_sch", "uuid": second_uuid}
        ]}})
        .to_string(),
    )
    .unwrap();

    let child = directory.path().join("shared.kicad_sch");
    create_blank_schematic(&child).unwrap();
    symbol(&child, "R1");
    let a = add(&first, "A", "shared.kicad_sch").await;
    let b = add(&second, "B", "shared.kicad_sch").await;
    let mut paths = [
        format!("/{first_uuid}/{}", a["sheet"]["uuid"].as_str().unwrap()),
        format!("/{second_uuid}/{}", b["sheet"]["uuid"].as_str().unwrap()),
    ];
    paths.sort();
    assert_instances(&child, &paths);
    assert_eq!(b["sheet"]["page"], "3");
}

#[tokio::test]
async fn shared_subtrees_survive_generated_repoint_delete_and_readd_sequences() {
    for seed in 1_u32..=8 {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("project.kicad_sch");
        let a = directory.path().join("a.kicad_sch");
        let b = directory.path().join("b.kicad_sch");
        let leaf = directory.path().join("leaf.kicad_sch");
        for path in [&root, &a, &b, &leaf] {
            create_blank_schematic(path).unwrap();
        }
        symbol(&leaf, "R1");
        for path in [&a, &b] {
            let mut child = cse::Schematic::load(path).unwrap();
            child.add_sheet(cse::Sheet::new(
                "Leaf",
                "leaf.kicad_sch",
                20.0,
                20.0,
                40.0,
                30.0,
            ));
            child.overwrite().unwrap();
        }
        std::fs::write(root.with_extension("kicad_pro"), "{}").unwrap();
        add(&root, "Permanent", "a.kicad_sch").await;
        let mut attached = false;
        let mut points_to_a = false;
        let mut state = seed;
        for _ in 0..12 {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            if !attached {
                points_to_a = state & 1 == 0;
                add(
                    &root,
                    "Rotating",
                    if points_to_a {
                        "a.kicad_sch"
                    } else {
                        "b.kicad_sch"
                    },
                )
                .await;
                attached = true;
            } else if state & 4 == 0 {
                decoded(
                    handle_delete_sheet(
                        &json!({"schematic": root, "sheet_name": "Rotating"}),
                        &context(),
                    )
                    .await
                    .unwrap(),
                );
                attached = false;
            } else {
                points_to_a = !points_to_a;
                decoded(
                    handle_edit_sheet(
                        &json!({
                            "schematic": root, "sheet_name": "Rotating",
                            "new_file": if points_to_a { "a.kicad_sch" } else { "b.kicad_sch" }
                        }),
                        &context(),
                    )
                    .await
                    .unwrap(),
                );
            }

            let root_doc = cse::Schematic::load(&root).unwrap();
            let root_id = root_doc.uuid.as_deref().unwrap();
            let mut expected = Vec::new();
            let mut parent_paths = std::collections::BTreeMap::<_, BTreeSet<_>>::new();
            let mut pages = BTreeSet::from(["1".to_string()]);
            for entry in root_doc.sheets.iter() {
                let child = cse::Schematic::load(directory.path().join(entry.file())).unwrap();
                let inner = child.sheets.by_name("Leaf").unwrap();
                let parent = format!("/{root_id}/{}", entry.uuid);
                parent_paths
                    .entry(entry.file())
                    .or_default()
                    .insert(parent.clone());
                expected.push(format!("{parent}/{}", inner.uuid));
                assert!(pages.insert(entry.page("project").unwrap().to_owned()));
            }
            for path in [&a, &b] {
                let child = cse::Schematic::load(path).unwrap();
                let inner = child.sheets.by_name("Leaf").unwrap();
                let actual = inner
                    .instances
                    .iter()
                    .map(|row| {
                        assert_eq!(row.project_name, "project");
                        assert!(pages.insert(row.page.clone()));
                        row.path.clone()
                    })
                    .collect::<BTreeSet<_>>();
                let filename = path.file_name().unwrap().to_str().unwrap();
                assert_eq!(actual, parent_paths.remove(filename).unwrap_or_default());
            }
            expected.sort();
            assert_instances(&leaf, &expected);
        }
    }
}

#[tokio::test]
async fn removing_the_primary_placement_keeps_the_surviving_reference() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project.kicad_sch");
    let leaf = directory.path().join("leaf.kicad_sch");
    create_blank_schematic(&root).unwrap();
    create_blank_schematic(&leaf).unwrap();
    symbol(&leaf, "R1");
    std::fs::write(root.with_extension("kicad_pro"), "{}").unwrap();
    let a = add(&root, "A", "leaf.kicad_sch").await;
    let b = add(&root, "B", "leaf.kicad_sch").await;
    let root_id = cse::Schematic::load(&root).unwrap().uuid.unwrap();
    let a_path = format!("/{root_id}/{}", a["sheet"]["uuid"].as_str().unwrap());
    let b_path = format!("/{root_id}/{}", b["sheet"]["uuid"].as_str().unwrap());
    let mut leaf_doc = cse::Schematic::load(&leaf).unwrap();
    leaf_doc.symbols[0].set_instance_path("project", &b_path, "R7", 1);
    leaf_doc.overwrite().unwrap();

    decoded(
        handle_delete_sheet(&json!({"schematic": root, "sheet_name": "A"}), &context())
            .await
            .unwrap(),
    );

    assert_instances(&leaf, std::slice::from_ref(&b_path));
    let leaf_doc = cse::Schematic::load(&leaf).unwrap();
    assert!(!leaf_doc.symbols[0].has_instance_path("project", &a_path));
    assert_eq!(leaf_doc.symbols[0].reference(), Some("R7"));
    assert_eq!(
        leaf_doc.symbols[0].instances()[0].reference.as_deref(),
        Some("R7")
    );
    assert_eq!(
        cse::Schematic::load(&root)
            .unwrap()
            .sheets
            .by_name("B")
            .unwrap()
            .page("project"),
        Some("3")
    );
}

#[tokio::test]
async fn repair_dry_run_and_apply_are_scoped_and_idempotent() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project.kicad_sch");
    let leaf = directory.path().join("leaf.kicad_sch");
    create_blank_schematic(&root).unwrap();
    std::fs::write(root.with_extension("kicad_pro"), "{}").unwrap();
    let added = add(&root, "Leaf", "leaf.kicad_sch").await;
    symbol(&leaf, "R1");
    let mut leaf_doc = cse::Schematic::load(&leaf).unwrap();
    leaf_doc.symbols[0].set_instance_path("obsolete", "/old", "R311", 1);
    leaf_doc.overwrite().unwrap();
    let before = std::fs::read(&leaf).unwrap();
    let root_before = std::fs::read(&root).unwrap();
    let report = decoded(
        handle_repair_sheet_instances(&json!({"schematic": leaf}), &context())
            .await
            .unwrap(),
    );
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["changed"], false);
    assert_eq!(report["symbol_instance_changes"], 1);
    assert!(report["diagnostics"][0]
        .as_str()
        .unwrap()
        .contains("obsolete:/old"));
    assert_eq!(std::fs::read(&leaf).unwrap(), before);

    let applied = decoded(
        handle_repair_sheet_instances(&json!({"schematic": leaf, "apply": true}), &context())
            .await
            .unwrap(),
    );
    assert_eq!(applied["changed"], true);
    assert_eq!(applied["files"].as_array().unwrap().len(), 1);
    assert_eq!(std::fs::read(&root).unwrap(), root_before);
    let root_id = cse::Schematic::load(&root).unwrap().uuid.unwrap();
    assert_instances(
        &leaf,
        &[format!(
            "/{root_id}/{}",
            added["sheet"]["uuid"].as_str().unwrap()
        )],
    );
    let repaired_bytes = std::fs::read(&leaf).unwrap();
    let repeated = decoded(
        handle_repair_sheet_instances(&json!({"schematic": leaf, "apply": true}), &context())
            .await
            .unwrap(),
    );
    assert_eq!(repeated["changed"], false);
    assert_eq!(std::fs::read(&leaf).unwrap(), repaired_bytes);
}

#[tokio::test]
async fn repair_refuses_a_foreign_project_row_without_editing_the_sheet() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project.kicad_sch");
    let foreign = directory.path().join("foreign.kicad_sch");
    let leaf = directory.path().join("leaf.kicad_sch");
    for root in [&root, &foreign] {
        create_blank_schematic(root).unwrap();
        std::fs::write(root.with_extension("kicad_pro"), "{}").unwrap();
    }
    add(&root, "Leaf", "leaf.kicad_sch").await;
    symbol(&leaf, "R1");
    let mut leaf_doc = cse::Schematic::load(&leaf).unwrap();
    leaf_doc.symbols[0].set_instance_path("foreign", "/old", "R11", 1);
    leaf_doc.overwrite().unwrap();
    let before = std::fs::read(&leaf).unwrap();
    let result =
        handle_repair_sheet_instances(&json!({"schematic": leaf, "apply": true}), &context()).await;
    assert!(result.is_err(), "{result:?}");
    assert_eq!(std::fs::read(&leaf).unwrap(), before);
}

#[tokio::test]
async fn graph_commit_refuses_changed_project_config_before_any_write() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project.kicad_sch");
    let child = directory.path().join("child.kicad_sch");
    let config = root.with_extension("kicad_pro");
    create_blank_schematic(&root).unwrap();
    std::fs::write(&config, "{}").unwrap();
    add(&root, "Child", "child.kicad_sch").await;
    let before = std::fs::read_to_string(&root).unwrap();
    let child_before = std::fs::read(&child).unwrap();
    let graph = HierarchySnapshot::load(&root, &before, &before, None).unwrap();
    let mut edited = cse::Schematic::from_source(&root, &before).unwrap();
    edited
        .sheets
        .by_name_mut("Child")
        .unwrap()
        .set_name("Changed");
    std::fs::write(&config, "{\"schematic\":{}}").unwrap();

    let result = graph.update(&root, edited.to_source(), None, None);
    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(&root).unwrap(), before);
    assert_eq!(std::fs::read(&child).unwrap(), child_before);
    assert!(konnect_sexp::inspect_file_transactions(directory.path())
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn repoint_rekeys_an_annotated_shared_leaf_and_its_nested_page() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project.kicad_sch");
    let a = directory.path().join("a.kicad_sch");
    let b = directory.path().join("b.kicad_sch");
    let leaf = directory.path().join("leaf.kicad_sch");
    let twig = directory.path().join("twig.kicad_sch");
    for file in [&root, &a, &b, &leaf, &twig] {
        create_blank_schematic(file).unwrap();
    }
    for file in [&a, &b] {
        let mut sch = cse::Schematic::load(file).unwrap();
        sch.add_sheet(cse::Sheet::new(
            "Leaf",
            "leaf.kicad_sch",
            20.0,
            20.0,
            40.0,
            30.0,
        ));
        sch.overwrite().unwrap();
    }
    let mut sch = cse::Schematic::load(&leaf).unwrap();
    sch.add_sheet(cse::Sheet::new(
        "Twig",
        "twig.kicad_sch",
        20.0,
        20.0,
        40.0,
        30.0,
    ));
    sch.overwrite().unwrap();
    symbol(&leaf, "R1");

    std::fs::write(root.with_extension("kicad_pro"), "{}").unwrap();
    let persistent = add(&root, "Persistent", "leaf.kicad_sch").await;
    let rotating = add(&root, "Rotating", "a.kicad_sch").await;
    let root_id = cse::Schematic::load(&root).unwrap().uuid.unwrap();
    let prefix = format!("/{root_id}/{}", rotating["sheet"]["uuid"].as_str().unwrap());
    let persistent = format!(
        "/{root_id}/{}",
        persistent["sheet"]["uuid"].as_str().unwrap()
    );
    let leaf_id = |path: &Path| {
        cse::Schematic::load(path)
            .unwrap()
            .sheets
            .by_name("Leaf")
            .unwrap()
            .uuid
            .clone()
    };
    let old_path = format!("{prefix}/{}", leaf_id(&a));
    let new_path = format!("{prefix}/{}", leaf_id(&b));
    let mut before = cse::Schematic::load(&leaf).unwrap();
    before.symbols[0].set_instance_path("project", &old_path, "R7", 1);
    before.symbols[0].set_instance_path("project", &new_path, "R3", 1);
    let old_page = before
        .sheets
        .by_name("Twig")
        .unwrap()
        .instances
        .iter()
        .find(|row| row.path == old_path)
        .unwrap()
        .page
        .clone();
    // Prior buggy metadata for the future path is not a live placement.
    before
        .sheets
        .by_name_mut("Twig")
        .unwrap()
        .set_page("project", &new_path, "99");
    before.overwrite().unwrap();

    decoded(
        handle_edit_sheet(
            &json!({"schematic": root, "sheet_name": "Rotating", "new_file": "b.kicad_sch"}),
            &context(),
        )
        .await
        .unwrap(),
    );

    let mut expected = [persistent, new_path.clone()];
    expected.sort();
    assert_instances(&leaf, &expected);
    let after = cse::Schematic::load(&leaf).unwrap();
    let instances = after.symbols[0].instances();
    assert!(instances
        .iter()
        .any(|row| row.path.as_ref() == Some(&new_path) && row.reference.as_deref() == Some("R7")));
    assert!(after
        .sheets
        .by_name("Twig")
        .unwrap()
        .instances
        .iter()
        .any(|row| row.path == new_path && row.page == old_page));
}

#[tokio::test]
async fn repoint_with_ambiguous_predecessor_placements_writes_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project.kicad_sch");
    let a = directory.path().join("a.kicad_sch");
    let b = directory.path().join("b.kicad_sch");
    let leaf = directory.path().join("leaf.kicad_sch");
    let files = [&root, &a, &b, &leaf];
    for file in files {
        create_blank_schematic(file).unwrap();
    }
    for file in [&a, &b] {
        let mut sch = cse::Schematic::load(file).unwrap();
        for name in ["First", "Second"] {
            sch.add_sheet(cse::Sheet::new(
                name,
                "leaf.kicad_sch",
                20.0,
                20.0,
                40.0,
                30.0,
            ));
        }
        sch.overwrite().unwrap();
    }
    symbol(&leaf, "R1");
    std::fs::write(root.with_extension("kicad_pro"), "{}").unwrap();
    add(&root, "Rotating", "a.kicad_sch").await;
    let expected = files.map(|file| std::fs::read(file).unwrap());

    let result = handle_edit_sheet(
        &json!({"schematic": root, "sheet_name": "Rotating", "new_file": "b.kicad_sch"}),
        &context(),
    )
    .await;
    assert!(result.is_err(), "{result:?}");
    assert_eq!(files.map(|file| std::fs::read(file).unwrap()), expected);
}

#[tokio::test]
async fn explicit_renumber_retains_the_saved_allocation_watermark() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project.kicad_sch");
    create_blank_schematic(&root).unwrap();
    std::fs::write(root.with_extension("kicad_pro"), "{}").unwrap();
    assert_eq!(add(&root, "A", "a.kicad_sch").await["sheet"]["page"], "2");
    assert_eq!(add(&root, "B", "b.kicad_sch").await["sheet"]["page"], "3");
    decoded(
        handle_delete_sheet(&json!({"schematic": root, "sheet_name": "A"}), &context())
            .await
            .unwrap(),
    );
    let history = directory.path().join(".konnect-sheet-pages.json");
    let before = std::fs::read(&history).unwrap();

    decoded(
        handle_renumber_sheet_pages(&json!({"schematic": root}), &context())
            .await
            .unwrap(),
    );

    let schematic = cse::Schematic::load(&root).unwrap();
    assert_eq!(
        schematic.sheets.by_name("B").unwrap().page("project"),
        Some("2")
    );
    assert_eq!(std::fs::read(history).unwrap(), before);
    assert_eq!(add(&root, "C", "c.kicad_sch").await["sheet"]["page"], "4");
}

#[tokio::test]
async fn invalid_page_history_blocks_topology_edits() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("project.kicad_sch");
    let history = directory.path().join(".konnect-sheet-pages.json");
    create_blank_schematic(&root).unwrap();
    let before = std::fs::read(&root).unwrap();
    let invalid = r#"{"version":9001,"allocations":[]}"#;
    std::fs::write(&history, invalid).unwrap();

    let result = handle_add_hierarchical_sheet(
        &json!({"schematic": root, "sheet_name": "A", "sheet_file": "a.kicad_sch"}),
        &context(),
    )
    .await;
    assert!(result.is_err());
    assert_eq!(std::fs::read(&root).unwrap(), before);
    assert_eq!(std::fs::read_to_string(history).unwrap(), invalid);
    assert!(!directory.path().join("a.kicad_sch").exists());
}

#[tokio::test]
async fn project_renumber_reserves_root_pages_and_preserves_previous_high_water() {
    let directory = tempfile::tempdir().unwrap();
    let first = directory.path().join("first.kicad_sch");
    let second = directory.path().join("second.kicad_sch");
    let shared = directory.path().join("shared.kicad_sch");
    let leaf = directory.path().join("leaf.kicad_sch");
    for path in [&first, &second, &shared, &leaf] {
        create_blank_schematic(path).unwrap();
    }
    let first_id = "4187d28e-1569-4eba-b9b7-e900f7d6d8e3";
    let second_id = "ef67c491-50cc-4611-9d4c-f378bfa497c0";
    std::fs::write(
        directory.path().join("project.kicad_pro"),
        json!({"schematic": {"top_level_sheets": [
            {"filename": "first.kicad_sch", "uuid": first_id},
            {"filename": "second.kicad_sch", "uuid": second_id},
        ]}})
        .to_string(),
    )
    .unwrap();

    let mut paths = Vec::new();
    for (file, id, page) in [(&first, first_id, "50"), (&second, second_id, "12")] {
        let mut schematic = cse::Schematic::load(file).unwrap();
        let mut sheet = cse::Sheet::new("Shared", "shared.kicad_sch", 20.0, 20.0, 40.0, 30.0);
        sheet.set_page("project", &format!("/{id}"), page);
        paths.push(format!("/{id}/{}", sheet.uuid));
        schematic.add_sheet(sheet);
        if file == &second {
            schematic
                .raw_other
                .retain(|node| node.tag() != Some("sheet_instances"));
            schematic.raw_other.push(
                cse::sexp::parser::parse(r#"(sheet_instances (path "/" (page "2")))"#).unwrap(),
            );
        }
        schematic.overwrite().unwrap();
    }
    let mut schematic = cse::Schematic::load(&shared).unwrap();
    schematic.add_sheet(cse::Sheet::new(
        "Leaf",
        "leaf.kicad_sch",
        20.0,
        20.0,
        40.0,
        30.0,
    ));
    schematic.overwrite().unwrap();
    assert!(!directory.path().join(".konnect-sheet-pages.json").exists());

    let result = decoded(
        handle_renumber_sheet_pages(&json!({"schematic": second}), &context())
            .await
            .unwrap(),
    );

    assert_eq!(result["scope"], "project");
    let first_doc = cse::Schematic::load(&first).unwrap();
    let first_rows = &first_doc.sheets.by_name("Shared").unwrap().instances;
    assert_eq!(first_rows[0].path, format!("/{first_id}"));
    assert_eq!(first_rows[0].page, "3");
    let second_doc = cse::Schematic::load(&second).unwrap();
    let second_rows = &second_doc.sheets.by_name("Shared").unwrap().instances;
    assert_eq!(second_rows[0].path, format!("/{second_id}"));
    assert_eq!(second_rows[0].page, "5");
    let shared_doc = cse::Schematic::load(&shared).unwrap();
    let rows = &shared_doc.sheets.by_name("Leaf").unwrap().instances;
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .any(|row| row.path == paths[0] && row.page == "4"));
    assert!(rows
        .iter()
        .any(|row| row.path == paths[1] && row.page == "6"));
    assert_eq!(
        add(&first, "After", "after.kicad_sch").await["sheet"]["page"],
        "51"
    );
}
