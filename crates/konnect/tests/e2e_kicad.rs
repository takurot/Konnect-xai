//! End-to-end test against a real KiCAD installation (kicad-cli).
//!
//! Drives the shipped binary over stdio through a full design loop:
//! create project → place components → wire → ERC → export Gerbers → DRC.
//! Also checks KiCad-backed PCB and schematic export tools through MCP.
//!
//! Requires kicad-cli and the standard symbol libraries, so it is `#[ignore]`
//! by default and run explicitly by the e2e-kicad workflow (and locally):
//!
//!     cargo test -p konnect --test e2e_kicad -- --ignored --nocapture

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    _state: tempfile::TempDir,
    next_id: i64,
}

/// Locate kicad-cli: KICAD_CLI env override, PATH, then platform defaults.
fn find_kicad_cli() -> Option<String> {
    if let Ok(p) = std::env::var("KICAD_CLI") {
        if std::path::Path::new(&p).exists() {
            return Some(p);
        }
    }
    let name = if cfg!(windows) {
        "kicad-cli.exe"
    } else {
        "kicad-cli"
    };
    if Command::new(name)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
    {
        return Some(name.to_string());
    }
    let candidates: &[&str] = if cfg!(windows) {
        &[
            r"C:\KiCad\10.0\bin\kicad-cli.exe",
            r"C:\Program Files\KiCad\10.0\bin\kicad-cli.exe",
        ]
    } else if cfg!(target_os = "macos") {
        &["/Applications/KiCad/KiCad.app/Contents/MacOS/kicad-cli"]
    } else {
        &["/usr/bin/kicad-cli", "/usr/local/bin/kicad-cli"]
    };
    candidates
        .iter()
        .find(|c| std::path::Path::new(c).exists())
        .map(|c| c.to_string())
}

impl Mcp {
    fn spawn(kicad_cli: &str) -> Self {
        Self::spawn_with_home(kicad_cli, None)
    }

    fn spawn_with_home(kicad_cli: &str, home: Option<&std::path::Path>) -> Self {
        let state = tempfile::tempdir().unwrap();
        let config_path = state.path().join("server.json");
        std::fs::write(&config_path, json!({"kicad_cli": kicad_cli}).to_string()).unwrap();

        let mut command = Command::new(env!("CARGO_BIN_EXE_konnect"));
        if let Some(home) = home {
            command.env("HOME", home).env("APPDATA", home);
        }
        let mut child = command
            .arg("--config")
            .arg(&config_path)
            .env("KONNECT_STATE_DIR", state.path().join("state"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let reader = BufReader::new(child.stdout.take().unwrap());
        let mut p = Mcp {
            child,
            stdin,
            reader,
            _state: state,
            next_id: 1,
        };
        p.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18", "capabilities": {},
                "clientInfo": {"name": "e2e", "version": "0"}
            }),
        );
        p
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        writeln!(
            self.stdin,
            "{}",
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
        )
        .unwrap();
        self.stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(self.reader.read_line(&mut line).unwrap() > 0, "server died");
            let v: Value = serde_json::from_str(line.trim()).unwrap();
            if v.get("id").and_then(Value::as_i64) == Some(id) {
                return v;
            }
        }
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        let r = self.request("tools/call", json!({"name": name, "arguments": args}));
        let result = r["result"].clone();
        assert_ne!(
            result["isError"],
            json!(true),
            "tool {name} failed: {}",
            result["content"][0]["text"].as_str().unwrap_or("?")
        );
        result
    }

    fn load(&mut self, toolset: &str) {
        self.tool("load_toolset", json!({"name": toolset}));
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn body(result: &Value) -> Value {
    serde_json::from_str(result["content"][0]["text"].as_str().unwrap_or("{}"))
        .unwrap_or(Value::Null)
}

#[test]
#[ignore = "requires kicad-cli; run via e2e workflow"]
fn fabrication_and_document_exports_through_mcp() {
    let kicad_cli = find_kicad_cli().expect("kicad-cli not found; set KICAD_CLI or install KiCad");
    let dir = tempfile::tempdir().unwrap();
    let fixtures =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../konnect-core/tests/fixtures");
    let board = dir.path().join("fabrication.kicad_pcb");
    let schematic = dir.path().join("document.kicad_sch");
    std::fs::copy(fixtures.join("specctra_soic8_roundrect.kicad_pcb"), &board).unwrap();
    std::fs::copy(fixtures.join("ecc83_multiunit.kicad_sch"), &schematic).unwrap();

    let mut mcp = Mcp::spawn(&kicad_cli);
    mcp.load("pcb_export");
    mcp.load("sch_export");

    let placement = dir.path().join("placement.csv");
    let response = body(&mcp.tool(
        "export_position_file",
        json!({"board":board,"output":placement,"side":"front","format":"csv"}),
    ));
    assert_eq!(response["output"], json!(placement));
    let csv = std::fs::read_to_string(placement).unwrap();
    let mut references: Vec<_> = csv
        .lines()
        .skip(1)
        .map(|line| line.split(',').next().unwrap().trim_matches('"'))
        .collect();
    references.sort_unstable();
    assert_eq!(references, ["U1", "U2"]);

    for (tool, filename, format, signature, payload) in [
        (
            "export_3d",
            "board.step",
            Some("step"),
            b"ISO-10303-21;".as_slice(),
            b"MANIFOLD_SOLID_BREP".as_slice(),
        ),
        (
            "export_3d",
            "board.wrl",
            Some("vrml"),
            b"#VRML V2.0".as_slice(),
            b"coordIndex".as_slice(),
        ),
        (
            "export_gencad",
            "board.cad",
            None,
            b"$HEADER".as_slice(),
            b"COMPONENT \"U1\"".as_slice(),
        ),
        (
            "export_ipc2581",
            "board.xml",
            None,
            b"<IPC-2581 ".as_slice(),
            b"refDes=\"U1\"".as_slice(),
        ),
        (
            "export_odb",
            "board.zip",
            None,
            b"PK\x03\x04".as_slice(),
            b"steps/pcb/profile".as_slice(),
        ),
    ] {
        let output = dir.path().join(filename);
        std::fs::write(&output, b"previous ordinary export").unwrap();
        let mut args = json!({"board":board,"output":output});
        if let Some(format) = format {
            args["format"] = json!(format);
        }
        let response = body(&mcp.tool(tool, args));
        assert_eq!(response["output"], json!(output), "{tool} {filename}");
        let contents = std::fs::read(&output).unwrap_or_else(|error| panic!("{filename}: {error}"));
        for expected in [signature, payload] {
            assert!(
                contents
                    .windows(expected.len())
                    .any(|bytes| bytes == expected),
                "missing {} in {filename}",
                String::from_utf8_lossy(expected)
            );
        }
    }

    let compressed_ipc = dir.path().join("packed-ipc.xml");
    mcp.tool(
        "export_ipc2581",
        json!({"board":board,"output":compressed_ipc,"compress":true}),
    );
    assert!(std::fs::read(compressed_ipc)
        .unwrap()
        .starts_with(b"PK\x03\x04"));

    let odb_dir = dir.path().join("odb-directory");
    std::fs::create_dir(&odb_dir).unwrap();
    mcp.tool(
        "export_odb",
        json!({"board":board,"output":odb_dir,"compression":"none"}),
    );
    assert!(odb_dir.join("steps/pcb/profile").is_file());

    let dxf_dir = dir.path().join("dxf");
    std::fs::create_dir(&dxf_dir).unwrap();
    let keep = dxf_dir.join("keep.txt");
    std::fs::write(&keep, b"unrelated output directory contents").unwrap();
    let response = body(&mcp.tool(
        "export_dxf",
        json!({"board":board,"output_dir":dxf_dir,"layers":["Edge.Cuts"]}),
    ));
    let files = response["files"].as_array().expect("DXF file inventory");
    assert_eq!(files.len(), 2);
    assert_eq!(
        std::fs::read(&keep).unwrap(),
        b"unrelated output directory contents"
    );
    let dxf_name = files
        .iter()
        .filter_map(Value::as_str)
        .find(|name| name.ends_with(".dxf"))
        .expect("DXF layer file");
    let dxf = std::fs::read_to_string(dxf_dir.join(dxf_name)).unwrap();
    assert!(dxf.lines().any(|line| line.trim() == "ENTITIES"));
    assert_eq!(dxf.lines().filter(|line| line.trim() == "LINE").count(), 4);

    let pdf = dir.path().join("schematic.pdf");
    let response = body(&mcp.tool(
        "export_schematic_pdf",
        json!({"schematic":schematic,"output":pdf}),
    ));
    assert_eq!(response["exported"], json!(pdf));
    assert!(std::fs::read(pdf).unwrap().starts_with(b"%PDF-"));
}

#[test]
#[ignore = "requires kicad-cli and standard symbol libraries; run via e2e workflow"]
fn native_assembly_attributes_survive_real_kicad_bom_export() {
    let kicad_cli = find_kicad_cli().expect("kicad-cli not found; set KICAD_CLI or install KiCad");
    let dir = tempfile::tempdir().unwrap();
    let sch = dir.path().join("ecc83-pp.kicad_sch");
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../konnect-core/tests/fixtures/ecc83_multiunit.kicad_sch"),
        &sch,
    )
    .unwrap();
    let export_references = |name: &str| {
        let output_path = dir.path().join(name);
        let output = Command::new(&kicad_cli)
            .args(["sch", "export", "bom", "--output"])
            .arg(&output_path)
            .args([
                "--fields",
                "Reference",
                "--labels",
                "Reference",
                "--group-by",
                "Reference",
                "--string-delimiter",
                "\"",
                "--exclude-dnp",
            ])
            .arg(&sch)
            .output()
            .expect("failed to run KiCad BOM exporter");
        assert!(
            output.status.success(),
            "KiCad BOM export failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::read_to_string(output_path)
            .unwrap()
            .lines()
            .skip(1)
            .map(|line| line.trim_matches('"').to_string())
            .collect::<Vec<_>>()
    };
    let resistor_control = "R1-R4";
    let before = export_references("before.csv");
    assert!(
        before.iter().any(|reference| reference == "U1"),
        "{before:?}"
    );
    assert!(
        before.iter().any(|reference| reference == resistor_control),
        "{before:?}"
    );

    let mut mcp = Mcp::spawn(&kicad_cli);
    mcp.load("sch_components");
    mcp.tool(
        "edit_schematic_component",
        json!({"schematic": sch.to_string_lossy(), "reference": "U1", "dnp": true}),
    );
    let saved = std::fs::read_to_string(&sch).unwrap();
    let tree = konnect_sexp::parse_sexp(&saved).unwrap();
    let units: Vec<_> = tree
        .find_all("symbol")
        .into_iter()
        .filter(|symbol| {
            symbol.find_all("property").iter().any(|property| {
                property.get(1).and_then(|value| value.as_str()) == Some("Reference")
                    && property.get(2).and_then(|value| value.as_str()) == Some("U1")
            })
        })
        .collect();
    assert_eq!(units.len(), 3, "all three U1 units must remain placed");
    for unit in units {
        assert_eq!(unit.find_str("dnp"), Some("yes"));
    }
    let after = export_references("after.csv");
    assert!(
        !after.iter().any(|reference| reference == "U1"),
        "{after:?}"
    );
    assert!(
        after.iter().any(|reference| reference == resistor_control),
        "{after:?}"
    );

    let project = dir.path().join("mounting-hole");
    mcp.tool(
        "create_project",
        json!({"name": "mounting-hole", "path": project.to_string_lossy()}),
    );
    let hole_sch = project.join("mounting-hole.kicad_sch");
    mcp.tool(
        "add_schematic_component",
        json!({"schematic": hole_sch.to_string_lossy(), "lib_id": "Mechanical:MountingHole",
            "reference": "H1", "x": 100.0, "y": 100.0}),
    );
    let saved = std::fs::read_to_string(hole_sch).unwrap();
    let tree = konnect_sexp::parse_sexp(&saved).unwrap();
    let placed = tree.find_all("symbol");
    assert_eq!(placed.len(), 1);
    let library = tree
        .find("lib_symbols")
        .unwrap()
        .find_all("symbol")
        .into_iter()
        .find(|symbol| {
            symbol.get(1).and_then(|value| value.as_str()) == Some("Mechanical:MountingHole")
        })
        .expect("the real installed MountingHole definition must be embedded");
    for symbol in [library, placed[0]] {
        assert_eq!(symbol.find_str("in_bom"), Some("no"));
        assert_eq!(symbol.find_str("on_board"), Some("yes"));
    }
}

#[test]
#[ignore = "requires kicad-cli; run via e2e workflow"]
fn selected_layer_pcb_svg_export_through_mcp() {
    let Some(kicad_cli) = find_kicad_cli() else {
        panic!("kicad-cli not found — set KICAD_CLI or install KiCAD (this test is e2e-only)");
    };
    // KiCad 10-reserialized fixture with a board-owned F.SilkS line and an
    // Edge.Cuts outline; see its README for the source and geometry.
    let board = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../konnect-core/tests/fixtures/drc_ownership_j1.kicad_pcb");
    let dir = tempfile::tempdir().unwrap();
    let both_path = dir.path().join("silk-and-edge.svg");
    let silk_path = dir.path().join("silk-only.svg");
    let mut mcp = Mcp::spawn(&kicad_cli);
    mcp.load("pcb_export");

    let both = body(&mcp.tool(
        "export_svg",
        json!({
            "board": board.to_string_lossy(),
            "output": both_path.to_string_lossy(),
            "layers": ["F.SilkS", "Edge.Cuts"]
        }),
    ));
    assert_eq!(both["success"], true, "{both}");
    let both_svg = std::fs::read_to_string(&both_path)
        .expect("selected-layer SVG was not written")
        .replace("\r\n", "\n");
    assert!(both_svg.contains("<svg") && both_svg.contains("</svg>"));
    assert!(both_svg.contains("M125.0000 60.0000\nL135.0000 60.0000"));
    assert!(both_svg.contains("M120.0000 56.5000\nL160.0000 56.5000"));

    let silk = body(&mcp.tool(
        "export_svg",
        json!({
            "board": board.to_string_lossy(),
            "output": silk_path.to_string_lossy(),
            "layers": ["F.SilkS"]
        }),
    ));
    assert_eq!(silk["success"], true, "{silk}");
    let silk_svg = std::fs::read_to_string(&silk_path)
        .expect("silkscreen SVG was not written")
        .replace("\r\n", "\n");
    assert!(silk_svg.contains("<svg") && silk_svg.contains("</svg>"));
    assert!(silk_svg.contains("M125.0000 60.0000\nL135.0000 60.0000"));
    assert!(!silk_svg.contains("M120.0000 56.5000\nL160.0000 56.5000"));
}

#[test]
#[ignore = "requires kicad-cli; run via e2e workflow"]
fn default_front_view_pcb_exports_through_mcp() {
    let cli = find_kicad_cli().expect("KiCad CLI required");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../konnect-core/tests/fixtures/drc_ownership_j1.kicad_pcb");
    let dir = tempfile::tempdir().unwrap();
    let board = dir.path().join("source.kicad_pcb");
    let original_board = std::fs::read_to_string(fixture)
        .unwrap()
        .replace(r#""F.Silkscreen""#, r#""Front legend custom""#);
    std::fs::write(&board, &original_board).unwrap();
    let library = dir.path().join("sibling.kicad_mod");
    std::fs::write(&library, r#"(footprint "existing")"#).unwrap();
    let mut mcp = Mcp::spawn(&cli);
    mcp.load("pcb_export");
    for (tool, name, magic) in [
        ("export_svg", "front.svg", b"<?xml".as_slice()),
        ("export_pdf", "front.pdf", b"%PDF-".as_slice()),
    ] {
        let output = dir.path().join(name);
        let response = body(&mcp.tool(tool, json!({"board":board,"output":output})));
        assert_eq!(response["success"], true, "{response}");
        assert_eq!(
            response["layers_used"],
            json!(["F.Cu", "F.Paste", "F.SilkS", "F.Mask", "Edge.Cuts"])
        );
        assert!(std::fs::read(&output).unwrap().starts_with(magic));

        let refused = mcp.request(
            "tools/call",
            json!({"name":tool,"arguments":{"board":board,"output":board}}),
        );
        assert_eq!(refused["result"]["isError"], true, "{refused}");
        assert_eq!(std::fs::read_to_string(&board).unwrap(), original_board);

        for args in [
            json!({"board":board,"output":library}),
            json!({"board":board,"output":output,"layers":["F.Cu", "not-a-layer"]}),
            json!({"board":board,"output":output,"layers":["F.Cu", "In1.Cu"]}),
        ] {
            let refused = mcp.request("tools/call", json!({"name":tool,"arguments":args}));
            assert_eq!(refused["result"]["isError"], true, "{refused}");
        }
        assert_eq!(
            std::fs::read_to_string(&library).unwrap(),
            r#"(footprint "existing")"#
        );
    }
    let alias = dir.path().join("alias.svg");
    let response = body(&mcp.tool(
        "export_svg",
        json!({
            "board":board,"output":alias,"layers":["Front legend custom"],
        }),
    ));
    assert_eq!(response["layers_used"], json!(["F.SilkS"]));
    let source = std::fs::read_to_string(alias)
        .unwrap()
        .replace("\r\n", "\n");
    assert!(source.contains("M125.0000 60.0000\nL135.0000 60.0000"));
}

#[test]
#[ignore = "requires kicad-cli; run via e2e workflow"]
fn named_schematic_svg_reports_root_and_hierarchy_outputs() {
    let cli = find_kicad_cli().expect("KiCad CLI required");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../konnect-core/tests/fixtures/sheet_pin_edges/sheet_pin_edges.kicad_sch");
    let dir = tempfile::tempdir().unwrap();
    let root_output = dir.path().join("custom.svg");
    let mut mcp = Mcp::spawn(&cli);
    mcp.load("sch_export");
    let response = body(&mcp.tool(
        "export_schematic_svg",
        json!({"schematic":fixture,"output":root_output}),
    ));
    assert_eq!(response["exported"], json!(root_output));
    let files = response["files"].as_array().unwrap();
    assert_eq!(files.len(), 2, "{response}");
    assert_eq!(files[0], json!(root_output));
    for output in files {
        let source = std::fs::read_to_string(output.as_str().unwrap()).unwrap();
        assert!(source.contains("<svg") && source.contains("</svg>"));
    }
    assert!(!dir.path().join("sheet_pin_edges.svg").exists());

    let child = std::path::Path::new(files[1].as_str().unwrap());
    assert_eq!(child.parent(), Some(dir.path()));
    std::fs::write(child, "existing child contents").unwrap();
    let original_root = std::fs::read(&root_output).unwrap();
    let refused = mcp.request(
        "tools/call",
        json!({
            "name":"export_schematic_svg",
            "arguments":{"schematic":fixture,"output":root_output}
        }),
    );
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert_eq!(
        std::fs::read_to_string(child).unwrap(),
        "existing child contents"
    );
    assert_eq!(std::fs::read(root_output).unwrap(), original_root);
    let alias = tempfile::tempdir().unwrap();
    let child_name = child.file_name().unwrap();
    let refused = mcp.request(
        "tools/call",
        json!({
            "name":"export_schematic_svg",
            "arguments":{"schematic":fixture,"output":alias.path().join(child_name)}
        }),
    );
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert_eq!(std::fs::read_dir(alias.path()).unwrap().count(), 0);
}

#[test]
#[ignore = "requires kicad-cli; run via e2e workflow"]
fn native_pcb_netlist_and_sibling_schematic_netlist_through_mcp() {
    let cli = find_kicad_cli().expect("KiCad CLI required");
    let fixtures =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../konnect-core/tests/fixtures");
    let dir = tempfile::tempdir().unwrap();
    let board = dir.path().join("ecc83-pp.kicad_pcb");
    let schematic = board.with_extension("kicad_sch");
    std::fs::copy(fixtures.join("drc_ownership_j1.kicad_pcb"), &board).unwrap();
    std::fs::copy(fixtures.join("ecc83_multiunit.kicad_sch"), &schematic).unwrap();
    let mut mcp = Mcp::spawn(&cli);
    mcp.load("pcb_export");

    let ipc = dir.path().join("board.d356");
    let response = body(&mcp.tool(
        "export_netlist",
        json!({"board":board,"output":ipc,"format":"ipc"}),
    ));
    assert_eq!(response["source"], json!(board));
    let ipc = std::fs::read_to_string(ipc).unwrap();
    // KiCad 10.0.5 writes CODE and UNITS, while 10.0.8 adds JOB and
    // pads its records to 80 columns. Both versions carry the same pins.
    assert!(ipc
        .lines()
        .any(|line| line.split_ascii_whitespace().eq(["P", "CODE", "00"])));
    assert!(ipc.lines().any(|line| line
        .split_ascii_whitespace()
        .eq(["P", "UNITS", "CUST", "0"])));
    for (record, pin) in [
        ("327GND", "-1"),
        ("327VCC", "-2"),
        ("327SCL", "-3"),
        ("327SDA", "-4"),
    ] {
        assert!(
            ipc.lines().any(|line| line
                .split_ascii_whitespace()
                .take(3)
                .eq([record, "J1", pin])),
            "missing IPC-D-356 pad J1{pin} on {record}",
        );
    }
    assert_eq!(ipc.lines().last().map(str::trim_end), Some("999"));

    let output = dir.path().join("design.net");
    let response = body(&mcp.tool("export_netlist", json!({"board":board,"output":output})));
    assert_eq!(response["source"], json!(schematic));
    let netlist = std::fs::read_to_string(output).unwrap();
    assert!(netlist.contains("(export") && netlist.contains("(components"));
    let rules = dir.path().join("sample.kicad_dru");
    std::fs::write(&rules, "(version 1)").unwrap();
    let refused = mcp.request(
        "tools/call",
        json!({
            "name":"export_netlist",
            "arguments":{"board":board,"output":rules,"format":"ipc"}
        }),
    );
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert_eq!(std::fs::read_to_string(rules).unwrap(), "(version 1)");
}

#[test]
#[ignore = "requires kicad-cli; run via e2e workflow"]
fn structural_scan_fixture_reloads_through_real_kicad() {
    let Some(kicad_cli) = find_kicad_cli() else {
        panic!("kicad-cli not found — set KICAD_CLI or install KiCAD (this test is e2e-only)");
    };
    let source = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../konnect-core/tests/fixtures/structural_scans_kicad10.kicad_sch");
    let directory = tempfile::tempdir().unwrap();
    let schematic = directory.path().join("structural_scans.kicad_sch");
    std::fs::copy(&source, &schematic).unwrap();

    let upgrade = Command::new(&kicad_cli)
        .args(["sch", "upgrade", "--force"])
        .arg(&schematic)
        .output()
        .expect("failed to run KiCad schematic upgrade");
    assert!(
        upgrade.status.success(),
        "KiCad could not reload and save the structural fixture: {}",
        String::from_utf8_lossy(&upgrade.stderr)
    );

    let netlist = directory.path().join("structural_scans.net");
    let export = Command::new(&kicad_cli)
        .args(["sch", "export", "netlist", "--output"])
        .arg(&netlist)
        .arg(&schematic)
        .output()
        .expect("failed to run KiCad netlist export");
    assert!(
        export.status.success(),
        "KiCad could not export the reloaded structural fixture: {}",
        String::from_utf8_lossy(&export.stderr)
    );
    let netlist = std::fs::read_to_string(netlist).expect("KiCad wrote no netlist");
    assert!(netlist.contains("R1") && netlist.contains("R3"));
}

#[test]
#[ignore = "requires kicad-cli + symbol libraries; run via e2e workflow"]
fn full_design_loop_with_real_kicad() {
    let Some(kicad_cli) = find_kicad_cli() else {
        panic!("kicad-cli not found — set KICAD_CLI or install KiCAD (this test is e2e-only)");
    };
    // KONNECT_E2E_KEEP_DIR: persist the generated project there (CI uploads
    // it as a failure artifact so file-format rejections can be diagnosed).
    let tmp = tempfile::tempdir().unwrap();
    let base: std::path::PathBuf = match std::env::var("KONNECT_E2E_KEEP_DIR") {
        Ok(d) => {
            std::fs::create_dir_all(&d).unwrap();
            d.into()
        }
        Err(_) => tmp.path().to_path_buf(),
    };
    let proj = base.join("e2e");
    let proj_s = proj.to_string_lossy().to_string();
    let sch = proj.join("e2e.kicad_sch");
    let pcb = proj.join("e2e.kicad_pcb");
    let mut p = Mcp::spawn(&kicad_cli);

    // ── Create ───────────────────────────────────────────────────────────
    p.tool("create_project", json!({"name": "e2e", "path": proj_s}));
    assert!(sch.exists() && pcb.exists());

    // ── Schematic: RC divider ────────────────────────────────────────────
    p.load("sch_components");
    p.load("sch_wiring");
    p.tool(
        "add_schematic_component",
        json!({
            "schematic": sch.to_string_lossy(), "lib_id": "Device:R",
            "reference": "R1", "value": "10k", "x": 100.0, "y": 100.0
        }),
    );
    p.tool(
        "add_schematic_component",
        json!({
            "schematic": sch.to_string_lossy(), "lib_id": "Device:C",
            // Pin 1 faces the incoming R1.2 route; the opposite orientation
            // passes over C1.2 first and would short the capacitor (#22).
            "reference": "C1", "value": "100n", "x": 120.0, "y": 100.0,
            "rotation": 180.0
        }),
    );
    p.tool(
        "add_schematic_component",
        json!({
            "schematic": sch.to_string_lossy(), "lib_id": "Regulator_Linear:LM7805_TO220",
            "reference": "U1", "x": 140.0, "y": 100.0
        }),
    );
    p.tool(
        "connect_pins",
        json!({
            "schematic": sch.to_string_lossy(),
            "ref1": "R1", "pin1": "2",
            "ref2": "C1", "pin2": "1"
        }),
    );

    // Labels are placed at the pin endpoint and oriented away from the symbol
    // body, so a rotated label must still bind its pin to the net. eeschema is
    // the only thing that can confirm that, hence here rather than a unit test.
    p.load("sch_batch");
    p.tool(
        "batch_connect_to_net",
        json!({
            "schematic": sch.to_string_lossy(), "net_name": "VIN",
            "pins": [{ "reference": "R1", "pin_number": "1" }]
        }),
    );

    // The written schematic must still parse and contain both parts.
    let content = std::fs::read_to_string(&sch).unwrap();
    let tree = konnect_sexp::parse_sexp(&content).expect("tool output must reparse");
    let refs: Vec<_> = konnect_sexp::schematic::extract_symbol_instances(&tree)
        .into_iter()
        .map(|s| s.reference)
        .collect();
    assert!(
        refs.contains(&"R1".to_string())
            && refs.contains(&"C1".to_string())
            && refs.contains(&"U1".to_string())
    );

    // KiCad's BOM exporter reads Datasheet and Description from the placed
    // instance, not the embedded lib_symbols fallback. Both rows must retain
    // the descriptions copied from the real Device library (#226).
    let bom_file = proj.join("metadata.csv");
    let bom_output = Command::new(&kicad_cli)
        .args(["sch", "export", "bom", "--output"])
        .arg(&bom_file)
        .args([
            "--fields",
            "Reference,Datasheet,Description",
            "--labels",
            "Reference,Datasheet,Description",
        ])
        .arg(&sch)
        .output()
        .expect("failed to run KiCad BOM exporter");
    assert!(
        bom_output.status.success(),
        "KiCad BOM export failed: {}",
        String::from_utf8_lossy(&bom_output.stderr)
    );
    let bom = std::fs::read_to_string(&bom_file).expect("KiCad wrote no BOM");
    for reference in ["R1", "C1", "U1"] {
        let prefix = format!("\"{reference}\",");
        let row = bom
            .lines()
            .find(|line| line.starts_with(&prefix))
            .unwrap_or_else(|| panic!("BOM has no {reference} row:\n{bom}"));
        assert!(
            !row.ends_with(",\"\""),
            "{reference} lost its library Description in KiCad's BOM:\n{bom}"
        );
    }
    assert!(
        !bom.lines().any(|line| line.starts_with("\"U1\",\"\",\"")),
        "U1 lost the regulator library Datasheet in KiCad's BOM:\n{bom}"
    );

    // ── ERC through real eeschema ────────────────────────────────────────
    p.load("sch_export");
    p.load("verification");
    let erc = body(&p.tool("run_erc", json!({"schematic": sch.to_string_lossy()})));
    // A 2-part net has floating-pin warnings; what matters is that eeschema
    // parsed OUR file and produced a structured report at all.
    assert!(
        erc.get("errors").is_some()
            || erc.get("violations").is_some()
            || erc.get("summary").is_some(),
        "unexpected ERC shape: {erc}"
    );
    // Every part on this sheet sits at x >= 100 mm, y = 100 mm, so a violation
    // on one of their pins cannot be at single-digit millimetres. That is what
    // KiCad's ERC JSON writer reported up to 10.0.6 — the whole sheet at 1/100
    // scale (#541, kicad#25582) — and this installation's version decides
    // which side of it the response is on.
    let coordinates = &erc["coordinates"];
    let status = coordinates["status"]
        .as_str()
        .unwrap_or_else(|| panic!("ERC response must state what its coordinates are worth: {erc}"));
    for violation in erc["violations"].as_array().into_iter().flatten() {
        for item in violation["items"].as_array().into_iter().flatten() {
            if status == "withheld" {
                assert!(
                    item["x"].is_null(),
                    "an unclassifiable version must offer no location: {item}"
                );
                continue;
            }
            let Some(x) = item["x"].as_f64() else {
                continue;
            };
            let y = item["y"].as_f64().expect("y rides with x");
            assert!(
                x > 10.0 && y > 10.0,
                "ERC placed a violation off the sheet at ({x}, {y}) — \
                 {status} against KiCad {:?}: {item}",
                coordinates["kicad_version"]
            );
        }
    }

    // eeschema's own netlist is the only proof that an oriented label still
    // binds its pin: rotating a label off 0° must not detach it.
    let net_file = proj.join("e2e.net");
    p.tool(
        "generate_netlist",
        json!({"schematic": sch.to_string_lossy(), "output": net_file.to_string_lossy()}),
    );
    let netlist = std::fs::read_to_string(&net_file).expect("kicad-cli wrote no netlist");
    // Not `netlist.contains("VIN")`: a label a millimetre off the pin still
    // names a net somewhere in the file. Only R1 pin 1 appearing as a node OF
    // that net proves the label bound the pin. eeschema may prefix the sheet
    // path to the name, so match it loosely.
    let vin = netlist
        .split("(net")
        .find(|block| {
            block
                .split_once("(name ")
                .and_then(|(_, rest)| rest.split_once(')'))
                .is_some_and(|(name, _)| name.contains("VIN"))
        })
        .unwrap_or_else(|| panic!("eeschema's netlist has no VIN net:\n{netlist}"));
    assert!(
        vin.split("(node")
            .any(|n| n.contains(r#"(ref "R1")"#) && n.contains(r#"(pin "1")"#)),
        "R1 pin 1 is not a node of VIN — the label did not bind its pin:\n{vin}"
    );

    // The review's runtime coverage uses Konnect's parser so the tool remains
    // usable without kicad-cli. Cross-check that count here against KiCad's
    // own netlist so parser drift cannot turn incomplete extraction into a
    // clean verdict (#184).
    let netlist_tree = konnect_sexp::parse_sexp(&netlist).expect("KiCad netlist must parse");
    let kicad_component_count = netlist_tree
        .find("components")
        .map(|components| components.find_all("comp").len())
        .unwrap_or(0);
    p.load("design_review");
    let review = body(&p.tool(
        "run_design_review",
        json!({"schematic": sch.to_string_lossy()}),
    ));
    assert_eq!(review["design_review"]["status"], "complete", "{review}");
    assert_eq!(
        review["design_review"]["coverage"]["schematic"]["symbol_instances"],
        json!(kicad_component_count),
        "Konnect and KiCad disagree on schematic component coverage: {review}"
    );

    // ── PCB: export Gerbers + DRC through real kicad-cli ─────────────────
    p.load("pcb_export");
    let out_dir = proj.join("gerbers");
    p.tool(
        "export_gerber",
        json!({
            "board": pcb.to_string_lossy(),
            "output_dir": out_dir.to_string_lossy()
        }),
    );
    let produced = std::fs::read_dir(&out_dir).map(|d| d.count()).unwrap_or(0);
    assert!(
        produced > 0,
        "no gerber files produced in {}",
        out_dir.display()
    );

    let drc = body(&p.tool("run_drc", json!({"board": pcb.to_string_lossy()})));
    assert!(
        drc.get("errors").is_some()
            || drc.get("violations").is_some()
            || drc.get("summary").is_some(),
        "unexpected DRC shape: {drc}"
    );

    eprintln!("E2E OK: project created, wired, ERC'd, {produced} gerber files, DRC'd");
}

/// #326 in plotted form: wire groups drawn with no stroke, and the junction
/// dot's radius, which collapses from 0.4572 mm to 0.0001 mm when the Default
/// has no `wire_width`. Neither shows up in ERC, so plotting is the only
/// headless way to see it.
fn plotted_wires_and_junction(kicad_cli: &str, sch: &std::path::Path) -> (usize, f64) {
    let out = sch.parent().unwrap().join("svg");
    let _ = std::fs::remove_dir_all(&out);
    let status = Command::new(kicad_cli)
        .args([
            "sch",
            "export",
            "svg",
            "--exclude-drawing-sheet",
            "--output",
        ])
        .arg(&out)
        .arg(sch)
        .status()
        .expect("kicad-cli sch export svg failed to run");
    assert!(status.success(), "kicad-cli sch export svg exited {status}");

    let svg_path = std::fs::read_dir(&out)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "svg"))
        .expect("kicad-cli wrote no SVG");
    let svg = std::fs::read_to_string(svg_path).unwrap();

    // Groups containing wires only. A healthy junction dot is filled rather
    // than stroked, so it carries `stroke:none` too.
    let mut strokeless = 0;
    let chunks: Vec<&str> = svg.split("style=\"").collect();
    for chunk in chunks.iter().skip(1) {
        let Some((style, body)) = chunk.split_once('"') else {
            continue;
        };
        if body.contains("<path d=") && style.contains("stroke:none") {
            strokeless += 1;
        }
    }

    // With two bare wires the only circle is the junction dot.
    let radius = svg
        .split("<circle")
        .skip(1)
        .filter_map(|c| c.split_once("r=\""))
        .filter_map(|(_, rest)| rest.split_once('"'))
        .filter_map(|(r, _)| r.parse::<f64>().ok())
        .fold(0.0_f64, f64::max);

    (strokeless, radius)
}

/// #326: a Default written with only the four PCB fields left eeschema unable
/// to place a junction anywhere in the project. The failure is in how KiCad
/// resolves the class rather than in the JSON, so only real eeschema can
/// confirm the fix.
#[test]
#[ignore = "requires kicad-cli; run via e2e workflow"]
fn a_written_default_netclass_still_plots_wires() {
    let Some(kicad_cli) = find_kicad_cli() else {
        panic!("kicad-cli not found — set KICAD_CLI or install KiCAD (this test is e2e-only)");
    };
    let tmp = tempfile::tempdir().unwrap();
    let proj = tmp.path().join("nc");
    let sch = proj.join("nc.kicad_sch");
    let pcb = proj.join("nc.kicad_pcb");
    let mut p = Mcp::spawn(&kicad_cli);

    p.tool(
        "create_project",
        json!({"name": "nc", "path": proj.to_string_lossy()}),
    );
    p.load("sch_wiring");
    // A T at (120, 100), where the junction dot belongs.
    p.tool(
        "batch_add_wire",
        json!({
            "schematic": sch.to_string_lossy(),
            "wires": [
                { "x1": 100.0, "y1": 100.0, "x2": 140.0, "y2": 100.0 },
                { "x1": 120.0, "y1": 100.0, "x2": 120.0, "y2": 120.0 }
            ]
        }),
    );
    // Baseline: no net_settings, so KiCad's seeded Default applies. The
    // radius comes from the wire width, which is what the assertions below
    // compare against.
    let (strokeless, radius) = plotted_wires_and_junction(&kicad_cli, &sch);
    assert_eq!(
        strokeless, 0,
        "a project with no net_settings must plot normally — the fixture is wrong, not the fix"
    );
    assert!(
        radius > 0.1,
        "no junction dot on the baseline plot (r={radius}) — the fixture is wrong, not the fix"
    );

    // The reported repro, exactly: name the Default and change nothing else.
    p.load("pcb_routing");
    p.tool(
        "create_netclass",
        json!({"board": pcb.to_string_lossy(), "name": "Default"}),
    );
    let (strokeless, after) = plotted_wires_and_junction(&kicad_cli, &sch);
    assert_eq!(
        strokeless, 0,
        "a Default written by create_netclass left eeschema with no wire width (#326)"
    );
    // Before the fix this collapsed to 0.0001 mm. Compared against the
    // baseline rather than a literal in case KiCad changes the ratio.
    assert!(
        (after - radius).abs() < 1e-6,
        "junction dot changed size after writing the Default: {radius} -> {after} (#326)"
    );

    // KiCad picks the default by name, not position, so a sparse named class
    // must not disturb it. Guards against "fixing" #326 by completing
    // whichever class comes first.
    p.tool(
        "create_netclass",
        json!({"board": pcb.to_string_lossy(), "name": "HV", "clearance": 0.5}),
    );
    let classes: Value =
        serde_json::from_str(&std::fs::read_to_string(proj.join("nc.kicad_pro")).unwrap()).unwrap();
    let names: Vec<&str> = classes["net_settings"]["classes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(names, vec!["Default", "HV"], "unexpected class order");
    let (strokeless, after) = plotted_wires_and_junction(&kicad_cli, &sch);
    assert_eq!(
        strokeless, 0,
        "a sparse named class must not disturb the Default"
    );
    assert!(
        (after - radius).abs() < 1e-6,
        "a sparse named class changed the junction dot: {radius} -> {after}"
    );
}

#[test]
#[ignore = "requires KiCad 10 and its stock symbol libraries"]
fn builtin_templates_place_with_complete_guides_on_stock_kicad() {
    use std::collections::{HashMap, HashSet};

    let cli = find_kicad_cli().expect("stock KiCad 10 required for template placement test");
    let dir = tempfile::tempdir().unwrap();
    let mut mcp = Mcp::spawn_with_home(&cli, Some(dir.path()));
    mcp.load("templates");
    mcp.load("sch_components");
    let tools = mcp.request("tools/list", json!({}));
    let description = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "apply_template")
        .unwrap()["description"]
        .as_str()
        .unwrap();
    assert!(description.contains("places components"), "{description}");
    assert!(description.contains("does not wire"), "{description}");

    let found = body(&mcp.tool("search_templates", json!({"query": ""})));
    let templates = found["templates"].as_array().unwrap();
    assert!(templates.len() >= 6, "{found}");
    for entry in templates {
        let id = entry["id"].as_str().unwrap();
        let project = dir.path().join(id);
        mcp.tool("create_project", json!({"name":id,"path":project}));
        let path = project.join(format!("{id}.kicad_sch"));
        let result = body(&mcp.tool(
            "apply_template",
            json!({"template_id":id,"schematic":path,"ref_start":100,"net_mappings":{"GND":"PROJECT_GND"}}),
        ));
        assert_eq!(result["template"], id);
        assert_eq!(result["wired"], false, "{result}");
        assert!(result["next_steps"]
            .as_str()
            .unwrap()
            .contains("No wires or labels were added"));

        let source = std::fs::read_to_string(&path).unwrap();
        let saved = konnect_sexp::parse_sexp(&source).unwrap();
        let symbols = saved.find_all("symbol");
        let placed = result["components_placed"].as_array().unwrap();
        let template = body(&mcp.tool("get_template", json!({"template_id":id})));
        let expected_count: u64 = template["components"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["quantity"].as_u64().unwrap_or(1))
            .sum();
        assert_eq!(placed.len(), expected_count as usize, "{id}");
        assert_eq!(symbols.len(), placed.len(), "{id}");
        for tag in [
            "wire",
            "junction",
            "label",
            "global_label",
            "hierarchical_label",
        ] {
            assert!(
                saved.find_all(tag).is_empty(),
                "{id} unexpectedly added {tag}"
            );
        }

        let netlist = project.join(format!("{id}.net"));
        let exported = Command::new(&cli)
            .args(["sch", "export", "netlist", "--output"])
            .arg(&netlist)
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            exported.status.success() && netlist.is_file(),
            "{id}: native KiCad could not read the saved placement: {}",
            String::from_utf8_lossy(&exported.stderr)
        );

        let mut pins = HashSet::new();
        let mut passive_pins = HashSet::new();
        for item in placed {
            let reference = item["reference"].as_str().unwrap();
            assert!(
                symbols.iter().any(|symbol| {
                    symbol.find_all("property").iter().any(|property| {
                        property.get(1).and_then(|value| value.as_str()) == Some("Reference")
                            && property.get(2).and_then(|value| value.as_str()) == Some(reference)
                    })
                }),
                "{id}: missing saved symbol {reference}"
            );
            let locations = body(&mcp.tool(
                "get_schematic_pin_locations",
                json!({"schematic":path,"reference":reference}),
            ));
            let numbers = locations["pins"].as_array().unwrap();
            assert!(!numbers.is_empty(), "{id}: {reference} has no pins");
            for pin in numbers {
                let endpoint = format!("{reference}.{}", pin["number"].as_str().unwrap());
                if numbers.len() == 2 {
                    passive_pins.insert(endpoint.clone());
                }
                pins.insert(endpoint);
            }
        }

        let mut covered = HashSet::new();
        let mut nets = HashMap::new();
        for row in result["connections_to_wire"].as_array().unwrap() {
            let from = row["from"].as_str().unwrap();
            assert!(pins.contains(from), "{id}: unknown source pin {from}");
            covered.insert(from.to_string());
            if let Some(to) = row["to"].as_str() {
                assert!(pins.contains(to), "{id}: unknown target pin {to}");
                covered.insert(to.to_string());
            } else {
                let net = row["to_net"].as_str().unwrap();
                assert!(
                    nets.insert(from, net).is_none(),
                    "{id}: repeated assignment for {from}"
                );
                assert_ne!(net, "GND", "{id}: ground alias was not applied");
                if net == "PROJECT_GND" {
                    assert_eq!(row["mapped_from"], "GND", "{id}: {row}");
                }
            }
        }
        if id != "i2c_pullups" {
            assert!(
                nets.values().any(|net| *net == "PROJECT_GND"),
                "{id}: no mapped ground"
            );
        }
        assert!(
            passive_pins.is_subset(&covered),
            "{id}: unwired guide pins {:?}",
            passive_pins.difference(&covered).collect::<Vec<_>>()
        );

        let required: &[(&str, &str)] = match id {
            "usb_c_5v_sink" => &[
                ("J100.A4", "VUSB"),
                ("J100.A9", "VUSB"),
                ("J100.B4", "VUSB"),
                ("J100.B9", "VUSB"),
                ("J100.A1", "PROJECT_GND"),
                ("J100.A12", "PROJECT_GND"),
                ("J100.B1", "PROJECT_GND"),
                ("J100.B12", "PROJECT_GND"),
                ("J100.A6", "USB_DP"),
                ("J100.B6", "USB_DP"),
                ("J100.A7", "USB_DN"),
                ("J100.B7", "USB_DN"),
                ("D100.1", "PROJECT_GND"),
                ("D100.2", "USB_DP"),
                ("D100.3", "USB_DN"),
                ("D100.4", "VUSB"),
            ],
            "stm32_minimal" => &[
                ("U100.24", "VCC_3V3"),
                ("U100.36", "VCC_3V3"),
                ("U100.48", "VCC_3V3"),
                ("U100.23", "PROJECT_GND"),
                ("U100.35", "PROJECT_GND"),
                ("U100.47", "PROJECT_GND"),
                ("U100.49", "PROJECT_GND"),
                ("U100.22", "VCAP"),
                ("U100.34", "SWDIO"),
                ("U100.37", "SWCLK"),
                ("J100.2", "SWDIO"),
                ("J100.4", "SWCLK"),
                ("J100.10", "NRST"),
            ],
            "ldo_3v3" => &[("U100.1", "VIN"), ("U100.3", "VIN"), ("U100.5", "VCC_3V3")],
            "buck_converter" => &[
                ("U100.4", "FB"),
                ("U100.6", "BOOT"),
                ("C104.1", "BOOT"),
                ("C104.2", "SW"),
            ],
            "i2c_pullups" => &[("R100.1", "SDA"), ("R101.1", "SCL")],
            "led_indicator" => &[("R100.1", "GPIO"), ("D100.1", "PROJECT_GND")],
            _ => &[],
        };
        for &(pin, net) in required {
            assert_eq!(nets.get(pin), Some(&net), "{id}: {pin}");
        }
    }
}
