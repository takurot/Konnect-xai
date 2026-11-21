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
            command
                .env("HOME", home)
                .env("APPDATA", home)
                .env("KICAD_CONFIG_HOME", home.join("kicad-config"));
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
#[ignore = "requires kicad-cli and stock libraries; run via e2e workflow"]
fn pcbway_package_excludes_schematic_dnp_from_bom_and_positions() {
    let cli = find_kicad_cli().expect("kicad-cli not found; set KICAD_CLI or install KiCad");
    let directory = tempfile::tempdir().unwrap();
    let project = directory.path().join("project");
    let schematic = project.join("assembly.kicad_sch");
    let board = project.join("assembly.kicad_pcb");
    let output = directory.path().join("package");
    let footprint = "Resistor_SMD:R_0603_1608Metric";
    let mut mcp = Mcp::spawn(&cli);
    mcp.tool(
        "create_project",
        json!({"name": "assembly", "path": project}),
    );
    mcp.load("sch_components");
    mcp.load("pcb_components");
    mcp.load("manufacturing");
    for (reference, x, dnp) in [("R1", 55.88, false), ("R2", 68.58, true)] {
        mcp.tool(
            "add_schematic_component",
            json!({
                "schematic": schematic, "lib_id": "Device:R", "reference": reference,
                "footprint": footprint, "x": x, "y": 50.8, "dnp": dnp
            }),
        );
        mcp.tool(
            "place_component",
            json!({
                "board": board, "footprint": footprint, "reference": reference, "x": x, "y": 50.8
            }),
        );
    }
    let saved = std::fs::read(&board).unwrap();
    let package = body(&mcp.tool(
        "export_manufacturing_package",
        json!({
            "board": board, "schematic": schematic, "output_dir": output, "fab_house": "pcbway",
            "bom_fields": "Reference,Value,Footprint,${QUANTITY}"
        }),
    ));
    assert_eq!(package["complete"], true, "{package}");
    for file in ["bom.csv", "positions.csv"] {
        let contents = std::fs::read_to_string(output.join(file)).unwrap();
        assert!(
            contents
                .lines()
                .any(|line| line.starts_with("R1,") || line.starts_with("\"R1\",")),
            "populated control R1 missing from {file}: {contents}"
        );
        assert!(
            !contents.contains("R2"),
            "unfitted R2 included in {file}: {contents}"
        );
    }
    assert_eq!(std::fs::read(board).unwrap(), saved);
}

#[test]
#[ignore = "requires kicad-cli; run via e2e workflow"]
fn schematic_calls_preserve_the_board_and_project_when_passed_the_wrong_file() {
    let kicad_cli = find_kicad_cli().expect("kicad-cli not found; set KICAD_CLI or install KiCad");
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("document_types");
    let board = project.join("document_types.kicad_pcb");
    let settings = project.join("document_types.kicad_pro");
    let schematic = project.join("document_types.kicad_sch");

    let mut mcp = Mcp::spawn(&kicad_cli);
    mcp.tool(
        "create_project",
        json!({"name": "document_types", "path": project}),
    );
    mcp.load("sch_wiring");
    mcp.load("sch_hierarchy");
    mcp.load("sch_analysis");

    for (tool, args, original) in [
        (
            "add_junction",
            json!({"schematic": board, "x": 50.8, "y": 50.8}),
            &board,
        ),
        (
            "set_schematic_page",
            json!({"schematic": board, "size": "A3"}),
            &board,
        ),
        (
            "list_schematic_nets",
            json!({"schematic": settings}),
            &settings,
        ),
    ] {
        let before = std::fs::read(original).unwrap();
        let response = mcp.request("tools/call", json!({"name": tool, "arguments": args}));
        let result = &response["result"];
        assert_eq!(result["isError"], true, "{response}");
        let refusal = body(result);
        assert_eq!(refusal["error"]["kind"], "invalid_argument", "{response}");
        assert_eq!(refusal["error"]["field"], "schematic", "{response}");
        assert_eq!(refusal["error"]["reason"], "not_a_schematic", "{response}");
        assert_eq!(std::fs::read(original).unwrap(), before);
    }

    mcp.tool(
        "set_schematic_page",
        json!({"schematic": schematic, "size": "A3"}),
    );
    let saved = std::fs::read_to_string(&schematic).unwrap();
    let document = konnect_sexp::parse_sexp(&saved).unwrap();
    assert_eq!(document.find_str("paper"), Some("A3"));

    for (domain, operation, input) in [("pcb", "drc", &board), ("sch", "erc", &schematic)] {
        let report = dir.path().join(format!("{domain}-report.txt"));
        let result = Command::new(&kicad_cli)
            .args([domain, operation, "--output"])
            .arg(&report)
            .arg(input)
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
        assert!(report.is_file(), "{report:?}");
    }
}

#[test]
#[ignore = "requires kicad-cli; run via e2e workflow"]
fn native_library_loading_survives_rejected_symbol_and_footprint_edits() {
    let kicad_cli = find_kicad_cli().expect("kicad-cli not found; set KICAD_CLI or install KiCad");
    let area = tempfile::tempdir().unwrap();
    let symbols = area.path().join("Parts.kicad_sym");
    let pads = area.path().join("Pads.pretty");
    let footprint = pads.join("Fixture.kicad_mod");
    let mut mcp = Mcp::spawn(&kicad_cli);
    mcp.load("library");

    mcp.tool(
        "create_symbol",
        json!({
            "library_path": symbols, "name": "Parent", "reference_prefix": "U",
            "pins": [{"number": "1", "name": "IN", "type": "input", "x": -5.08, "y": 0, "angle": 0}]
        }),
    );
    let mut source = std::fs::read_to_string(&symbols).unwrap();
    let close = source.rfind(')').unwrap();
    source.insert_str(
        close,
        r#"(symbol "Dependent"
            (extends "Parent")
            (property "Value" "Dependent" (at 0 -2.54 0) (effects (font (size 1.27 1.27)))))
        "#,
    );
    std::fs::write(&symbols, &source).unwrap();

    let response = mcp.request(
        "tools/call",
        json!({
            "name": "delete_symbol",
            "arguments": {"library_path": symbols, "symbol_name": "Parent"}
        }),
    );
    assert_eq!(response["result"]["isError"], true, "{response}");
    let error = body(&response["result"]);
    assert!(
        error["error"]["reason"]
            .as_str()
            .unwrap()
            .contains("Dependent"),
        "{error}"
    );
    assert_eq!(std::fs::read_to_string(&symbols).unwrap(), source);

    mcp.tool(
        "create_footprint",
        json!({
            "output": footprint, "name": "Fixture",
            "pads": [{"number":"1","type":"smd","shape":"rect","x":0,"y":0,"width":2,"height":2}]
        }),
    );
    let original = std::fs::read(&footprint).unwrap();
    let response = mcp.request(
        "tools/call",
        json!({
            "name": "edit_footprint_pad",
            "arguments": {"footprint_path": footprint, "pad_number": "1", "width": -2}
        }),
    );
    assert_eq!(response["result"]["isError"], true, "{response}");
    assert_eq!(
        body(&response["result"])["error"]["kind"],
        "invalid_argument"
    );
    assert_eq!(std::fs::read(&footprint).unwrap(), original);

    let upgraded_symbols = area.path().join("Upgraded.kicad_sym");
    let upgraded_pads = area.path().join("Upgraded.pretty");
    for (mode, source, target) in [
        ("sym", &symbols, &upgraded_symbols),
        ("fp", &pads, &upgraded_pads),
    ] {
        let result = Command::new(&kicad_cli)
            .args([mode, "upgrade", "--force", "--output"])
            .arg(target)
            .arg(source)
            .output()
            .unwrap();
        assert!(result.status.success(), "{result:?}");
    }
    assert!(upgraded_symbols.is_file());
    assert!(upgraded_pads.join("Fixture.kicad_mod").is_file());
}

#[test]
#[ignore = "requires kicad-cli; run via e2e workflow"]
fn placement_courtyards_agree_with_real_kicad_on_rotated_demo_board() {
    let cli = find_kicad_cli().expect("kicad-cli not found; set KICAD_CLI or install KiCad");
    let dir = tempfile::tempdir().unwrap();
    let board = dir.path().join("ecc83-pp.kicad_pcb");
    let original = include_bytes!("../../konnect-sexp/tests/fixtures/ecc83-pp.kicad_pcb");
    std::fs::write(&board, original).unwrap();
    let report = dir.path().join("drc.json");
    let native = Command::new(&cli)
        .args(["pcb", "drc", "--format", "json", "-o"])
        .arg(&report)
        .arg(&board)
        .output()
        .unwrap();
    assert!(native.status.success(), "{native:?}");
    let drc: Value = serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
    assert!(drc["violations"]
        .as_array()
        .unwrap()
        .iter()
        .all(|violation| violation["type"] != "courtyards_overlap"));

    let mut mcp = Mcp::spawn(&cli);
    mcp.load("placement");
    let score = body(&mcp.tool("score_placement", json!({"board": board})));
    assert!(
        score["hard_failures"]
            .as_array()
            .unwrap()
            .iter()
            .all(|failure| failure["kind"] != "courtyard_overlap"),
        "{score}"
    );
    assert_eq!(std::fs::read(&board).unwrap(), original);
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
#[ignore = "requires kicad-cli and symbol libraries; run via e2e workflow"]
fn normalized_schematic_rotations_load_in_real_kicad() {
    use konnect_sexp::schematic::{extract_labels, extract_symbol_instances, read_schematic};

    let kicad_cli = find_kicad_cli().expect("kicad-cli is required");
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("rotation");
    let path = project.join("rotation.kicad_sch");
    let mut mcp = Mcp::spawn(&kicad_cli);
    mcp.tool(
        "create_project",
        json!({"name": "rotation", "path": project}),
    );
    mcp.load("sch_components");
    mcp.load("sch_wiring");
    mcp.tool(
        "add_schematic_component",
        json!({"schematic": path, "lib_id": "Device:R", "reference": "R1", "x": 50.8, "y": 50.8}),
    );
    for rotation in [-90, 270] {
        let rotated = body(&mcp.tool(
            "rotate_schematic_component",
            json!({"schematic": path, "reference": "R1", "rotation": rotation}),
        ));
        assert_eq!(rotated["rotation"], 270.0, "{rotated}");
    }
    mcp.tool(
        "add_power_symbol",
        json!({"schematic": path, "power_net": "GND", "x": 55.88, "y": 55.88, "rotation": -90}),
    );
    mcp.tool(
        "add_schematic_net_label",
        json!({"schematic": path, "net": "SIGNAL", "label_type": "global_label",
            "x": 76.2, "y": 76.2, "rotation": 630}),
    );
    let (_, tree) = read_schematic(&path).unwrap();
    let symbols = extract_symbol_instances(&tree);
    assert_eq!(symbols.len(), 2);
    assert!(symbols.iter().all(|symbol| symbol.rotation == 270.0));
    let labels = extract_labels(&tree);
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].rotation, 270.0);

    let report = tmp.path().join("erc.json");
    let output = Command::new(&kicad_cli)
        .args(["sch", "erc", "--format", "json", "--output"])
        .arg(&report)
        .arg(&path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "KiCad ERC rejected the schematic: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&std::fs::read(report).unwrap()).unwrap();
    assert!(report["sheets"].is_array(), "{report}");
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

    // The review counts schematic placements using Konnect's parser.
    // Cross-check that count here against KiCad's
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
#[ignore = "requires KiCad and its stock library templates"]
fn library_search_works_without_global_tables_and_includes_project_footprints() {
    let cli = find_kicad_cli().expect("stock KiCad required for library search");
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let project = workspace.path().join("fixture.kicad_pro");
    let footprint_table = workspace.path().join("fp-lib-table");
    let library = workspace.path().join("Custom.pretty");
    let footprint = library.join("MySOT.kicad_mod");
    std::fs::write(&project, "{}").unwrap();
    std::fs::create_dir(&library).unwrap();
    std::fs::write(
        &footprint,
        r#"(footprint "MySOT" (version 20241229) (generator "pcbnew") (layer "F.Cu"))"#,
    )
    .unwrap();
    let table = r#"(fp_lib_table (version 7) (lib (name "Custom") (type "KiCad") (uri "${KIPRJMOD}/Custom.pretty") (options "") (descr "")))"#;
    std::fs::write(&footprint_table, table).unwrap();

    let mut mcp = Mcp::spawn_with_home(&cli, Some(home.path()));
    mcp.load("library");
    for (tool, query, expected) in [
        ("search_symbols", "LM358", "Amplifier_Operational:LM358"),
        (
            "search_footprints",
            "R_0603_1608",
            "Resistor_SMD:R_0603_1608Metric",
        ),
    ] {
        let search = body(&mcp.tool(tool, json!({"query":query})));
        assert_eq!(search["table_source"], "template", "{search}");
        assert!(
            search["results"]
                .as_array()
                .unwrap()
                .iter()
                .any(|hit| hit["id"] == expected),
            "{tool} should find {expected}: {search}"
        );
    }
    for tool in ["list_symbol_libraries", "list_footprint_libraries"] {
        let found = body(&mcp.tool(tool, json!({"scope":"global"})));
        assert_eq!(found["table_source"], "template", "{found}");
        assert!(found["count"].as_u64().unwrap() > 0, "{found}");
    }
    let found = body(&mcp.tool(
        "search_footprints",
        json!({"query":"MySOT","project":project,"limit":1}),
    ));
    assert_eq!(found["results"][0]["id"], "Custom:MySOT", "{found}");
    let info = body(&mcp.tool(
        "get_footprint_info",
        json!({"footprint_path":"Custom:MySOT","project":project}),
    ));
    assert_eq!(info["name"], "MySOT");
    assert_eq!(std::fs::read_to_string(&footprint_table).unwrap(), table);
}

#[test]
#[ignore = "requires KiCad 10 and its stock symbol and footprint libraries"]
fn design_review_and_readiness_use_native_evidence() {
    let cli = find_kicad_cli().expect("KiCad 10 required for design review");
    let dir = tempfile::tempdir().unwrap();
    let mut mcp = Mcp::spawn(&cli);
    for toolset in [
        "sch_components",
        "sch_wiring",
        "design_review",
        "verification",
        "pcb_board",
        "pcb_components",
        "manufacturing",
    ] {
        mcp.load(toolset);
    }
    let gates = dir.path().join("gates");
    mcp.tool("create_project", json!({"name": "gates", "path": gates}));
    let schematic = gates.join("gates.kicad_sch");
    let board = gates.join("gates.kicad_pcb");
    for (reference, x, y) in [("U1", 50.8, 50.8), ("U2", 76.2, 76.2)] {
        mcp.tool(
            "add_schematic_component",
            json!({
                "schematic": schematic, "lib_id": "74xGxx:74LVC1G04", "reference": reference,
                "footprint": "Package_TO_SOT_SMD:SOT-23-5", "x": x, "y": y
            }),
        );
        mcp.tool(
            "edit_schematic_component",
            json!({
                "schematic": schematic, "reference": reference, "fields": {"MPN": "SN74LVC1G04DBV"}
            }),
        );
    }
    mcp.tool(
        "connect_pins",
        json!({
            "schematic": schematic, "ref1": "U1", "pin1": "4", "ref2": "U2", "pin2": "4"
        }),
    );
    for (cap, gate, x, y) in [("C1", "U1", 45.72, 31.75), ("C2", "U2", 71.12, 57.15)] {
        mcp.tool(
            "add_schematic_component",
            json!({
                "schematic": schematic, "lib_id": "Device:C", "reference": cap,
                "footprint": "Capacitor_SMD:C_0603_1608Metric", "value": "100n", "x": x, "y": y
            }),
        );
        mcp.tool(
            "connect_pins",
            json!({
                "schematic": schematic, "ref1": cap, "pin1": "2", "ref2": gate, "pin2": "5"
            }),
        );
    }
    mcp.tool(
        "add_schematic_net_label",
        json!({
            "schematic": schematic, "net": "OUTPUTS", "x": 63.5, "y": 50.8
        }),
    );
    let standalone = body(&mcp.tool("audit_connections", json!({"schematic": schematic})));
    assert!(standalone["coverage"]["not_checked"]
        .as_array()
        .unwrap()
        .contains(&json!("shorted_outputs")));
    let unified = body(&mcp.tool("run_design_review", json!({"schematic": schematic})));
    let review = &unified["design_review"];
    assert_eq!(review["status"], "complete", "{review}");
    assert!(
        review["verdict"].as_str().unwrap().starts_with("NOT READY"),
        "{review}"
    );
    assert!(
        review["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["audit"] == "erc"
                && finding["rule"] == "pin_to_pin"
                && finding["severity"] == "error"),
        "{review}"
    );

    let dfm = body(&mcp.tool("audit_manufacturing", json!({"board": board})));
    assert!(
        dfm["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["issue"]
                .as_str()
                .unwrap()
                .contains("No board outline")),
        "{dfm}"
    );
    mcp.tool(
        "set_design_rules",
        json!({"board": board, "min_trace_width": 0.08}),
    );
    let dfm = body(&mcp.tool("audit_manufacturing", json!({"board": board})));
    assert!(
        dfm["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|finding| finding["issue"]
                .as_str()
                .unwrap()
                .contains("Minimum trace width (0.080mm)")),
        "{dfm}"
    );

    let pairs = dir.path().join("pairs");
    mcp.tool("create_project", json!({"name": "pairs", "path": pairs}));
    let schematic = pairs.join("pairs.kicad_sch");
    let board = pairs.join("pairs.kicad_pcb");
    let footprint = "Resistor_SMD:R_0603_1608Metric";
    for (reference, x) in [("R1", 50.8), ("R2", 76.2)] {
        mcp.tool(
            "add_schematic_component",
            json!({
                "schematic": schematic, "lib_id": "Device:R", "reference": reference,
                "footprint": footprint, "x": x, "y": 50.8
            }),
        );
    }
    for (pin, net, y) in [("1", "SUPPLY", 46.99), ("2", "RETURN", 54.61)] {
        mcp.tool(
            "connect_pins",
            json!({
                "schematic": schematic, "ref1": "R1", "pin1": pin, "ref2": "R2", "pin2": pin
            }),
        );
        mcp.tool(
            "add_schematic_net_label",
            json!({"schematic": schematic, "net": net, "x": 50.8, "y": y}),
        );
    }
    mcp.tool(
        "add_board_outline",
        json!({"board": board, "x1": 20.0, "y1": 20.0, "x2": 70.0, "y2": 70.0}),
    );
    for (reference, x) in [("R1", 30.0), ("R2", 50.0)] {
        mcp.tool(
            "place_component",
            json!({
                "board": board, "footprint": footprint, "reference": reference, "x": x, "y": 35.0
            }),
        );
    }
    mcp.tool(
        "set_design_rules",
        json!({"board": board, "min_trace_width": 0.20}),
    );
    let readiness = body(&mcp.tool("validate_for_manufacturing", json!({"board": board})));
    assert_eq!(readiness["board_info"]["net_count"], 0, "{readiness}");
    assert!(
        readiness["drc"]["schematic_parity"]
            .as_u64()
            .is_some_and(|count| count > 0),
        "{readiness}"
    );
    assert_ne!(readiness["verdict"], "READY", "{readiness}");
    assert!(
        readiness["issues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|issue| issue["issue"]
                .as_str()
                .unwrap()
                .starts_with("DRC schematic parity")),
        "{readiness}"
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
