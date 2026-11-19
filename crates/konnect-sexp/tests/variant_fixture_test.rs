//! Guards the KiCad 10 variant-grammar fixture (fixtures/variants/) — the
//! evidence base for the upcoming variants toolset. The grammar was
//! adjudicated by kicad-cli acceptance (see the fixture README); these tests
//! keep the fixture parseable and its facts pinned, and the `#[ignore]` test
//! re-runs the adjudication against a live kicad-cli.

use konnect_sexp::parse_sexp;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/variants")
        .join(name)
}

fn schematic_source() -> String {
    std::fs::read_to_string(fixture("ecc83-pp.kicad_sch")).unwrap()
}

#[test]
fn variant_fixture_parses() {
    let tree = parse_sexp(&schematic_source()).expect("KiCad-authored fixture must parse");
    assert_eq!(tree.head(), Some("kicad_sch"));
}

/// The variant clause sits inside R1's instance path block, carrying the two
/// verified override kinds: population (dnp) and a field override (Value).
#[test]
fn fixture_carries_the_verified_variant_clause() {
    let source = schematic_source();
    let clause_start = source
        .find("(variant (name \"Lite\")")
        .expect("R1's Lite clause present");
    let clause = &source[clause_start..clause_start + 200];
    assert!(clause.contains("(dnp yes)"), "population override present");
    assert!(
        clause.contains("(field (name \"Value\") (value \"22k\"))"),
        "field override present — native variants carry field overrides"
    );

    // And it is inside R1's instances path, not floating elsewhere.
    let r1_path = source
        .find("(reference \"R1\")")
        .expect("R1 instance present");
    assert!(
        clause_start > r1_path && clause_start - r1_path < 300,
        "clause belongs to R1's instance block"
    );
}

#[test]
fn project_file_registers_the_variant_name() {
    let pro = std::fs::read_to_string(fixture("ecc83-pp.kicad_pro")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&pro).unwrap();
    assert_eq!(json["variants"], serde_json::json!(["Lite"]));
}

/// Golden facts from the kicad-cli adjudication, pinned so a fixture edit
/// cannot silently invalidate the evidence the README cites.
#[test]
fn goldens_pin_the_adjudicated_facts() {
    let default = std::fs::read_to_string(fixture("bom_default.golden.csv")).unwrap();
    let lite = std::fs::read_to_string(fixture("bom_lite.golden.csv")).unwrap();
    assert!(
        default.contains("\"R1\",\"1.5K\",\"\""),
        "default: fitted, base value"
    );
    assert!(
        lite.contains("\"R1\",\"22k\",\"DNP\""),
        "Lite: DNP + overridden value"
    );
}

/// One row's value and DNP display column. The latter is localized by KiCad.
type Bom = BTreeMap<String, (String, String)>;

fn read_bom(path: &Path) -> Bom {
    let mut reader = csv::Reader::from_path(path).unwrap();
    assert_eq!(
        reader.headers().unwrap().iter().collect::<Vec<_>>(),
        ["Reference", "Value", "DNP"],
        "unexpected BOM columns in {path:?}"
    );
    let mut bom = Bom::new();
    for row in reader.records() {
        let row = row.unwrap_or_else(|error| panic!("invalid BOM row in {path:?}: {error}"));
        let previous = bom.insert(row[0].to_owned(), (row[1].to_owned(), row[2].to_owned()));
        assert!(
            previous.is_none(),
            "duplicate reference in {path:?}: {row:?}"
        );
    }
    bom
}

fn export_bom(cli: &str, schematic: &Path, label: &str, options: &[&str]) -> Bom {
    let output_path = schematic.with_file_name(format!("{label}.csv"));
    let output = Command::new(cli)
        .args(["sch", "export", "bom"])
        .args(options)
        .args([
            "--fields",
            "Reference,Value,${DNP}",
            "--labels",
            "Reference,Value,DNP",
            "--output",
        ])
        .arg(&output_path)
        .arg(schematic)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "kicad-cli refused {label}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let bom = read_bom(&output_path);
    println!("kicad-cli wrote {label} BOM with {} references", bom.len());
    bom
}

fn verify_variant_bom(default: &Bom, lite: &Bom, excluded: &Bom) -> Result<(), &'static str> {
    if !default
        .get("R1")
        .is_some_and(|(value, dnp)| value == "1.5K" && dnp.is_empty())
    {
        return Err("default R1 lost its base value or fitted state");
    }
    let Some((value, dnp)) = lite.get("R1") else {
        return Err("Lite R1 is missing from the unfiltered BOM");
    };
    if value != "22k" {
        return Err("Lite R1 value override was not applied");
    }
    if excluded.contains_key("R1") {
        return Err("Lite --exclude-dnp did not remove R1");
    }
    if dnp.is_empty() {
        return Err("Lite R1 is not marked DNP");
    }
    // R1 is the only overridden part; make a no-op filter or a filter that
    // drops all rows fail independently of the translated DNP indicator.
    let mut expected_fitted = default.clone();
    expected_fitted.remove("R1");
    if expected_fitted.is_empty() || *excluded != expected_fitted {
        return Err("Lite --exclude-dnp changed unrelated BOM rows");
    }
    Ok(())
}

/// Live re-adjudication with KiCad's own unfiltered and population-filtered
/// BOM exports. An explicitly requested live test must execute the CLI.
#[test]
#[ignore = "requires an installed kicad-cli; run via e2e-kicad or locally"]
fn kicad_cli_still_honors_the_variant_clause() {
    let cli = ["C:/KiCad/10.0/bin/kicad-cli.exe", "kicad-cli"]
        .into_iter()
        .find(|candidate| {
            Command::new(candidate)
                .arg("--version")
                .output()
                .is_ok_and(|result| result.status.success())
        })
        .expect("kicad-cli is required for the requested variant acceptance test");

    let dir = tempfile::tempdir().unwrap();
    for name in ["ecc83-pp.kicad_sch", "ecc83-pp.kicad_pro"] {
        std::fs::copy(fixture(name), dir.path().join(name)).unwrap();
    }
    let schematic = dir.path().join("ecc83-pp.kicad_sch");
    let default = export_bom(cli, &schematic, "default", &[]);
    let lite = export_bom(cli, &schematic, "lite", &["--variant", "Lite"]);
    let filtered = ["--variant", "Lite", "--exclude-dnp"];
    let excluded = export_bom(cli, &schematic, "lite-exclude-dnp", &filtered);
    verify_variant_bom(&default, &lite, &excluded).unwrap_or_else(|error| {
        panic!("{error}: default={default:?}, Lite={lite:?}, filtered={excluded:?}")
    });

    // Change one override at a time on the disposable copy and use real CLI
    // output to prove that each regression would fail for the expected reason.
    let source = schematic_source();
    let value_clause = r#"(field (name "Value") (value "22k"))"#;
    assert_eq!(source.matches(value_clause).count(), 1);
    std::fs::write(
        &schematic,
        source.replace(value_clause, r#"(field (name "Value") (value "12k"))"#),
    )
    .unwrap();
    let wrong_value = export_bom(cli, &schematic, "lite-wrong-value", &["--variant", "Lite"]);
    assert_eq!(
        verify_variant_bom(&default, &wrong_value, &excluded),
        Err("Lite R1 value override was not applied")
    );

    let population_clause = "(dnp yes)";
    assert_eq!(source.matches(population_clause).count(), 1);
    std::fs::write(&schematic, source.replace(population_clause, "(dnp no)")).unwrap();
    let wrong_population = export_bom(cli, &schematic, "lite-wrong-population", &filtered);
    assert_eq!(
        verify_variant_bom(&default, &lite, &wrong_population),
        Err("Lite --exclude-dnp did not remove R1")
    );
}
