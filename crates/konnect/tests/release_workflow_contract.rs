//! Release safety depends on a small set of GitHub Actions edges that Rust's
//! compiler cannot see. Keep those edges executable so a workflow cleanup
//! cannot silently make real-KiCad acceptance advisory again (#572).

use std::path::{Path, PathBuf};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("konnect crate must live below the repository root")
        .to_path_buf()
}

fn workflow(name: &str) -> serde_json::Value {
    let source = std::fs::read_to_string(repository_root().join(".github/workflows").join(name))
        .unwrap_or_else(|error| panic!("failed to read {name}: {error}"));
    serde_yaml_ng::from_str(&source)
        .unwrap_or_else(|error| panic!("failed to parse {name} as YAML: {error}"))
}

#[test]
fn release_publication_requires_real_kicad_acceptance() {
    let release = workflow("release.yml");

    assert_eq!(
        release["jobs"]["real-kicad-acceptance"]["uses"], "./.github/workflows/e2e-kicad.yml",
        "release.yml must call the real-KiCad acceptance workflow"
    );
    assert_eq!(
        release["jobs"]["release"]["needs"],
        serde_json::json!([
            "real-kicad-acceptance",
            "build",
            "pcm-package",
            "linux-compatibility"
        ]),
        "the release publication job must require real-KiCad acceptance and both artifact jobs"
    );
}

#[test]
fn release_workflow_has_a_non_publishing_pre_tag_entry_point() {
    let release = workflow("release.yml");

    assert!(
        release["on"].get("workflow_dispatch").is_some(),
        "maintainers need to exercise the release graph before creating a tag"
    );
    assert_eq!(
        release["jobs"]["release"]["if"],
        "github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')",
        "a manual release smoke run must never publish a GitHub release"
    );
    let steps = release["jobs"]["publish-npm"]["steps"].as_array().unwrap();
    let publish = steps.iter().find(|step| step["name"] == "Publish").unwrap();
    assert_eq!(
        publish["if"], "github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')",
        "a manual run must avoid registry version and authentication checks"
    );
    assert!(steps.iter().any(|step| {
        step["if"] == "github.event_name == 'workflow_dispatch'"
            && step["run"]
                .as_str()
                .is_some_and(|script| script.contains("npm pack --dry-run"))
    }));
}

#[test]
fn release_pcm_validation_uses_the_effective_release_version() {
    let release = workflow("release.yml");
    let validation = release["jobs"]["pcm-package"]["steps"]
        .as_array()
        .expect("PCM package steps must be an array")
        .iter()
        .find(|step| step["name"] == "Validate PCM package against KiCAD schema (release gate)")
        .expect("release workflow must validate PCM packages");
    let script = validation["run"]
        .as_str()
        .expect("PCM validation step must have a shell script");

    assert!(
        script.contains("version=\"${RELEASE_VERSION#v}\""),
        "PCM validation must use the effective version shared by tag and dry-run builds"
    );
    assert!(
        !script.contains("GITHUB_REF_NAME"),
        "a workflow_dispatch run uses the main branch ref, not the synthetic package version"
    );
}

#[test]
fn workflows_do_not_depend_on_node20_setup_protoc() {
    for name in ["ci.yml", "e2e-kicad.yml", "release.yml"] {
        let source =
            std::fs::read_to_string(repository_root().join(".github/workflows").join(name))
                .unwrap_or_else(|error| panic!("failed to read {name}: {error}"));

        assert!(
            !source.contains("arduino/setup-protoc"),
            "{name} must not reintroduce the Node-20 setup-protoc action"
        );
        assert!(
            source.contains("tool: protoc@3.23.4") && source.contains("fallback: none"),
            "{name} must install the reviewed protoc version without an unreviewed fallback"
        );
    }
}

#[test]
fn real_kicad_workflow_remains_reusable_and_opt_in_for_pull_requests() {
    let e2e = workflow("e2e-kicad.yml");

    let triggers = &e2e["on"];
    assert!(
        triggers.get("workflow_call").is_some(),
        "the release workflow needs a reusable real-KiCad workflow"
    );
    assert_eq!(
        triggers["pull_request"]["types"],
        serde_json::json!(["labeled"]),
        "the pull-request entry point must react only to label events"
    );
    assert_eq!(
        e2e["jobs"]["e2e"]["if"],
        "github.event_name != 'pull_request' || github.event.label.name == 'run:e2e-kicad'",
        "pull requests must run real-KiCad acceptance only through the run:e2e-kicad label"
    );
    assert!(
        triggers.get("schedule").is_some() && triggers.get("workflow_dispatch").is_some(),
        "weekly and manual real-KiCad entry points must remain available"
    );
}

#[test]
fn linux_release_artifacts_use_the_documented_glibc_floor() {
    let release = workflow("release.yml");
    for (job, selector, value, runner) in [
        (
            "build",
            "target",
            "x86_64-unknown-linux-gnu",
            "ubuntu-22.04",
        ),
        (
            "build",
            "target",
            "aarch64-unknown-linux-gnu",
            "ubuntu-22.04-arm",
        ),
        ("pcm-package", "platform", "linux", "ubuntu-22.04"),
    ] {
        let row = release["jobs"][job]["strategy"]["matrix"]["include"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row[selector] == value)
            .unwrap();
        assert_eq!(row["os"], runner, "{job} {value}");
        assert!(release["jobs"][job]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|step| step["run"]
                .as_str()
                .is_some_and(|script| script.contains("packaging/check-glibc.sh"))));
    }
}

#[test]
fn release_jobs_require_native_linux_compatibility_smoke() {
    let release = workflow("release.yml");
    for job in ["release", "publish-npm"] {
        assert!(release["jobs"][job]["needs"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("linux-compatibility")));
    }
    let smoke = &release["jobs"]["linux-compatibility"];
    assert_eq!(
        smoke["strategy"]["matrix"]["image"],
        serde_json::json!(["ubuntu:22.04", "debian:12"])
    );
    assert_eq!(
        smoke["strategy"]["matrix"]["os"],
        serde_json::json!(["ubuntu-22.04", "ubuntu-22.04-arm"])
    );
}

#[test]
fn license_and_advisory_gates_cover_both_lockfiles() {
    let ci = workflow("ci.yml");
    assert!(ci["on"].get("schedule").is_some());
    for job in ["licenses", "advisories"] {
        let steps = ci["jobs"][job]["steps"].as_array().unwrap();
        assert!(steps
            .iter()
            .any(|step| step["with"]["command"] == format!("check {job}")
                && step["with"]["manifest-path"] == "crates/schematic-viewer/Cargo.toml"));
    }
}
