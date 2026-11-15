use super::*;
use konnect_schematic_editor::library::SymbolLibrarySource;

fn text(result: &CallToolResult) -> String {
    format!("{:?}", result.content)
}

#[test]
fn lib_ids_need_a_nickname_of_two_or_more_characters_and_no_separators() {
    assert!(is_lib_id("Resistor_SMD:R_0402"));
    assert!(is_lib_id("AB:x"));
    assert!(is_lib_id("1:x"), "a digit prefix is not a drive letter");
    assert!(!is_lib_id("C:R.kicad_mod"), "one letter reads as a drive");
    assert!(!is_lib_id("plain_name"));
    assert!(!is_lib_id("dir/Lib:Part"));
    assert!(!is_lib_id("dir\\Lib:Part"));
}

#[test]
fn portable_uris_drop_the_verbatim_prefix_and_use_forward_slashes() {
    assert_eq!(portable_uri(Path::new(r"\\?\C:\proj\lib")), "C:/proj/lib");
    assert_eq!(portable_uri(Path::new("/a/b")), "/a/b");
}

#[test]
fn project_relative_uri_is_kiprjmod_inside_the_project_and_absolute_outside() {
    let project = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let inside = project.path().join("libs/mine.pretty");
    std::fs::create_dir_all(&inside).unwrap();
    assert_eq!(
        project_relative_uri(&inside, project.path()),
        "${KIPRJMOD}/libs/mine.pretty"
    );
    let outside = elsewhere.path().join("their.pretty");
    std::fs::create_dir_all(&outside).unwrap();
    let uri = project_relative_uri(&outside, project.path());
    assert!(!uri.starts_with("${KIPRJMOD}"), "{uri}");
    assert!(uri.ends_with("their.pretty"), "{uri}");
}

#[test]
fn project_relative_uri_handles_a_library_that_does_not_exist_yet() {
    let project = tempfile::tempdir().unwrap();
    let future = project.path().join("new.pretty");
    assert_eq!(
        project_relative_uri(&future, project.path()),
        "${KIPRJMOD}/new.pretty"
    );
    let uri = project_relative_uri(Path::new("/nowhere/new.pretty"), project.path());
    assert_eq!(uri, "/nowhere/new.pretty");
}

#[test]
fn project_relative_uri_never_emits_kiprjmod_for_an_empty_table_dir() {
    assert_eq!(
        project_relative_uri(Path::new("/abs/lib.pretty"), Path::new("")),
        "/abs/lib.pretty"
    );
}

#[test]
fn lexical_relative_path_climbs_out_of_the_table_dir() {
    assert_eq!(
        lexical_relative_path(Path::new("/r/proj/sub"), Path::new("/r/libs/a.pretty")).unwrap(),
        PathBuf::from("../../libs/a.pretty")
    );
    assert_eq!(
        lexical_relative_path(Path::new("/r/proj"), Path::new("/r/proj/a")).unwrap(),
        PathBuf::from("a")
    );
    assert!(lexical_relative_path(Path::new("rel/a"), Path::new("/abs/b")).is_none());
    assert!(lexical_relative_path(Path::new("/a"), Path::new("/b")).is_some());
}

#[test]
fn repository_relative_uri_reaches_a_sibling_library_inside_the_same_repository() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir(repo.path().join(".git")).unwrap();
    let table_dir = repo.path().join("hw/proj");
    let library = repo.path().join("libs/shared.pretty");
    std::fs::create_dir_all(&table_dir).unwrap();
    std::fs::create_dir_all(&library).unwrap();
    let pro = table_dir.join("proj.kicad_pro");
    std::fs::write(&pro, "{}").unwrap();
    assert_eq!(
        repository_relative_uri(&library, &table_dir, &pro).as_deref(),
        Some("${KIPRJMOD}/../../libs/shared.pretty")
    );
}

#[test]
fn repository_relative_uri_refuses_without_a_project_file_or_repository() {
    let repo = tempfile::tempdir().unwrap();
    let table_dir = repo.path().join("proj");
    let library = repo.path().join("lib.pretty");
    std::fs::create_dir_all(&table_dir).unwrap();
    std::fs::create_dir_all(&library).unwrap();
    let pro = table_dir.join("proj.kicad_pro");
    assert!(
        repository_relative_uri(&library, &table_dir, &pro).is_none(),
        "no project file on disk"
    );
    std::fs::write(&pro, "{}").unwrap();
    assert!(
        repository_relative_uri(&library, &table_dir, &pro).is_none(),
        "no .git above the table"
    );
    let not_pro = table_dir.join("notes.txt");
    std::fs::write(&not_pro, "").unwrap();
    assert!(repository_relative_uri(&library, &table_dir, &not_pro).is_none());
    std::fs::create_dir(repo.path().join(".git")).unwrap();
    let missing_lib = repo.path().join("absent.pretty");
    assert!(repository_relative_uri(&missing_lib, &table_dir, &pro).is_none());
}

#[test]
fn repository_relative_uri_refuses_a_library_outside_the_repository() {
    let repo = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    std::fs::create_dir(repo.path().join(".git")).unwrap();
    let table_dir = repo.path().join("proj");
    std::fs::create_dir_all(&table_dir).unwrap();
    let pro = table_dir.join("proj.kicad_pro");
    std::fs::write(&pro, "{}").unwrap();
    let library = other.path().join("lib.pretty");
    std::fs::create_dir_all(&library).unwrap();
    assert!(repository_relative_uri(&library, &table_dir, &pro).is_none());
}

#[test]
fn project_table_dir_accepts_directories_and_project_files_and_refuses_guesses() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        project_table_dir(&dir.path().display().to_string()).unwrap(),
        dir.path()
    );
    let pro = dir.path().join("p.kicad_pro");
    assert_eq!(
        project_table_dir(&pro.display().to_string()).unwrap(),
        dir.path(),
        "a not-yet-created .kicad_pro still names its directory"
    );
    assert_eq!(
        project_table_dir("bare.kicad_pro").unwrap(),
        PathBuf::from("."),
        "an empty parent falls back to the current directory"
    );
    let plain = dir.path().join("plain.txt");
    std::fs::write(&plain, "").unwrap();
    assert_eq!(
        project_table_dir(&plain.display().to_string()).unwrap(),
        dir.path()
    );
    let error = project_table_dir("no-such-thing").unwrap_err();
    assert!(error.is_error);
    assert!(text(&error).contains("'no-such-thing'"), "{}", text(&error));
}

#[test]
fn lib_table_target_routes_global_and_project_scopes_and_refuses_a_missing_project() {
    let project = tempfile::tempdir().unwrap();
    let lib = project.path().join("mine.pretty");
    std::fs::create_dir_all(&lib).unwrap();
    let global = PathBuf::from("/cfg/fp-lib-table");

    let (table, uri) =
        lib_table_target("global", None, &lib, global.clone(), "fp-lib-table").unwrap();
    assert_eq!(table, global);
    assert!(uri.ends_with("mine.pretty") && !uri.starts_with("${KIPRJMOD}"));

    let error =
        lib_table_target("project", None, &lib, global.clone(), "fp-lib-table").unwrap_err();
    assert!(
        text(&error).contains("provide 'project'"),
        "{}",
        text(&error)
    );

    let proj = project.path().display().to_string();
    let (table, uri) =
        lib_table_target("project", Some(&proj), &lib, global.clone(), "fp-lib-table").unwrap();
    assert_eq!(table, project.path().join("fp-lib-table"));
    assert_eq!(uri, "${KIPRJMOD}/mine.pretty");

    let error =
        lib_table_target("project", Some("missing-dir"), &lib, global, "fp-lib-table").unwrap_err();
    assert!(text(&error).contains("'missing-dir'"), "{}", text(&error));
}

#[test]
fn lib_table_target_uses_a_repository_relative_uri_for_a_library_outside_the_project_dir() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::create_dir(repo.path().join(".git")).unwrap();
    let table_dir = repo.path().join("hw");
    let library = repo.path().join("libs/shared.pretty");
    std::fs::create_dir_all(&table_dir).unwrap();
    std::fs::create_dir_all(&library).unwrap();
    let pro = table_dir.join("hw.kicad_pro");
    std::fs::write(&pro, "{}").unwrap();
    let (_, uri) = lib_table_target(
        "project",
        Some(&pro.display().to_string()),
        &library,
        PathBuf::from("/cfg/fp-lib-table"),
        "fp-lib-table",
    )
    .unwrap();
    assert_eq!(uri, "${KIPRJMOD}/../libs/shared.pretty");
}

#[test]
fn expand_lib_uri_passes_plain_uris_and_resolves_kiprjmod_only_when_it_exists() {
    assert_eq!(
        expand_lib_uri("/abs/lib.pretty", None),
        Some(PathBuf::from("/abs/lib.pretty"))
    );
    assert_eq!(expand_lib_uri("", None), None);
    assert_eq!(expand_lib_uri("${unterminated", None), None);

    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join("mine.pretty")).unwrap();
    assert_eq!(
        expand_lib_uri("${KIPRJMOD}/mine.pretty", Some(project.path())),
        Some(project.path().join("mine.pretty"))
    );
    assert_eq!(
        expand_lib_uri("${KIPRJMOD}/absent.pretty", Some(project.path())),
        None
    );
    assert_eq!(expand_lib_uri("${KIPRJMOD}/x", None), None);
}

#[test]
fn expand_lib_uri_leaves_an_unknown_variable_unresolved() {
    assert_eq!(
        expand_lib_uri("${KONNECT_TEST_UNSET_VAR}/x.pretty", None),
        None
    );
    assert_eq!(
        expand_lib_uri("${KONNECT_TEST_UNSET_FOOTPRINT_DIR}/NoSuch.pretty", None),
        None
    );
    assert_eq!(
        expand_lib_uri("${KONNECT_TEST_UNSET_SYMBOL_DIR}/NoSuch.kicad_sym", None),
        None
    );
    assert_eq!(
        expand_lib_uri("${KONNECT_TEST_UNSET_3DMODEL_DIR}/NoSuch.step", None),
        None
    );
}

#[test]
fn parse_lib_table_reads_escaped_quotes_and_skips_nothing_well_formed() {
    let table = "(fp_lib_table\n\t(lib (name \"A\") (type \"KiCad\") (uri \"/p/a.pretty\") (descr \"say \\\"hi\\\"\"))\n\t(lib (name \"B\") (type \"KiCad\") (uri \"/p/b.pretty\"))\n)";
    let libs = parse_lib_table(table);
    assert_eq!(libs.len(), 2);
    assert_eq!(libs[0]["nickname"], "A");
    assert_eq!(libs[0]["description"], "say \"hi\"");
    assert_eq!(libs[1]["uri"], "/p/b.pretty");
    assert_eq!(libs[1]["description"], "");
}

#[test]
fn flatten_lib_table_follows_nested_tables_and_attaches_resolved_paths() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("nested-table");
    std::fs::write(
        &nested,
        "(fp_lib_table (lib (name \"Inner\") (type \"KiCad\") (uri \"/inner.pretty\")))",
    )
    .unwrap();
    let outer = format!(
        "(fp_lib_table (lib (name \"Outer\") (type \"KiCad\") (uri \"/outer.pretty\")) \
         (lib (name \"Ref\") (type \"Table\") (uri \"{}\")) \
         (lib (name \"Gone\") (type \"Table\") (uri \"/no/such/table\")) \
         (lib (name \"Unres\") (type \"KiCad\") (uri \"${{KONNECT_TEST_UNSET_VAR}}/x\")))",
        sexp_path(&nested)
    );
    let libs = flatten_lib_table(&outer, 0, None);
    let names: Vec<&str> = libs.iter().filter_map(|l| l["nickname"].as_str()).collect();
    assert_eq!(
        names,
        ["Outer", "Inner", "Unres"],
        "unreadable nested table is dropped"
    );
    assert_eq!(libs[0]["path"], "/outer.pretty");
    assert_eq!(libs[1]["path"], "/inner.pretty");
    assert!(
        libs[2].get("path").is_none(),
        "unresolved URI carries no path"
    );
}

#[test]
fn flatten_lib_table_stops_following_tables_past_the_depth_limit() {
    let dir = tempfile::tempdir().unwrap();
    let nested = dir.path().join("deep");
    std::fs::write(
        &nested,
        "(fp_lib_table (lib (name \"Deep\") (type \"KiCad\") (uri \"/deep.pretty\")))",
    )
    .unwrap();
    let outer = format!(
        "(fp_lib_table (lib (name \"Ref\") (type \"Table\") (uri \"{}\")))",
        sexp_path(&nested)
    );
    assert_eq!(flatten_lib_table(&outer, 0, None).len(), 1);
    assert!(flatten_lib_table(&outer, MAX_LIB_TABLE_DEPTH, None).is_empty());
}

#[test]
fn read_lib_table_checked_tells_absent_from_unreadable() {
    let dir = tempfile::tempdir().unwrap();
    assert!(read_lib_table_checked(&dir.path().join("absent"))
        .unwrap()
        .is_empty());
    let error = read_lib_table_checked(dir.path()).unwrap_err();
    assert!(error.starts_with("Cannot read lib-table"), "{error}");
}

fn project_with_fp_table(entries: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("fp-lib-table"),
        format!("(fp_lib_table\n{entries})\n"),
    )
    .unwrap();
    dir
}

#[test]
fn resolve_footprint_path_reads_a_direct_path_or_names_the_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("R.kicad_mod");
    std::fs::write(&file, "").unwrap();
    assert_eq!(
        resolve_footprint_path(&file.display().to_string(), None).unwrap(),
        file
    );
    let error = resolve_footprint_path(
        &dir.path().join("absent.kicad_mod").display().to_string(),
        None,
    )
    .unwrap_err();
    assert!(error.contains("Footprint file not found"), "{error}");
}

#[test]
fn resolve_footprint_path_prefers_the_project_table_entry() {
    let libs = tempfile::tempdir().unwrap();
    let pretty = libs.path().join("Mine.pretty");
    std::fs::create_dir(&pretty).unwrap();
    std::fs::write(pretty.join("R_X.kicad_mod"), "").unwrap();
    let project = project_with_fp_table(&format!(
        "(lib (name \"Mine\") (type \"KiCad\") (uri \"{}\"))\n(lib (name \"Broken\") (type \"KiCad\") (uri \"${{KONNECT_TEST_UNSET_VAR}}/x\"))\n",
        sexp_path(&pretty)
    ));
    assert_eq!(
        resolve_footprint_path("Mine:R_X", Some(project.path())).unwrap(),
        pretty.join("R_X.kicad_mod")
    );
    let missing = resolve_footprint_path("Mine:Absent", Some(project.path())).unwrap_err();
    assert!(
        missing.contains("Footprint 'Absent' not found in library 'Mine'"),
        "{missing}"
    );
    let unresolvable = resolve_footprint_path("Broken:Any", Some(project.path())).unwrap_err();
    assert!(
        unresolvable.contains("Library 'Broken' has an unresolvable URI"),
        "{unresolvable}"
    );
}

#[test]
fn resolve_footprint_path_names_the_project_libraries_when_the_nickname_is_unknown() {
    let project =
        project_with_fp_table("(lib (name \"Known\") (type \"KiCad\") (uri \"/k.pretty\"))\n");
    let error = resolve_footprint_path("Nope_Nickname_X:Part", Some(project.path())).unwrap_err();
    assert!(
        error.contains("Library 'Nope_Nickname_X' not found in the project or global fp-lib-table"),
        "{error}"
    );
    assert!(
        error.contains("e.g. Known"),
        "known libraries are listed: {error}"
    );
}

#[test]
fn symbol_source_lists_project_table_paths_before_install_directories() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("sym-lib-table"),
        "(sym_lib_table (lib (name \"Mine\") (type \"KiCad\") (uri \"/syms/mine.kicad_sym\")))",
    )
    .unwrap();
    let source = KiCadSymbolSource::new(Some(project.path().to_path_buf()));
    let candidates = source.candidates("Mine");
    assert_eq!(candidates[0], PathBuf::from("/syms/mine.kicad_sym"));
    assert!(KiCadSymbolSource::new(None)
        .candidates("Konnect_Test_No_Such_Lib")
        .iter()
        .all(|c| c.to_string_lossy().contains("Konnect_Test_No_Such_Lib")));
}

#[test]
fn a_project_root_is_the_directory_holding_a_library_table() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("sym-lib-table"), "(sym_lib_table)").unwrap();
    let sheet = project.path().join("a.kicad_sch");
    assert_eq!(
        project_root_for(&sheet).unwrap().as_deref(),
        Some(project.path())
    );
}

/// Escapes a path for embedding in a quoted s-expression string, where `\\` is an escape
/// character (Windows separators would otherwise be consumed).
fn sexp_path(path: &std::path::Path) -> String {
    path.display().to_string().replace('\\', "\\\\")
}
