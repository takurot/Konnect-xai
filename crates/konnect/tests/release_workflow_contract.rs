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

const TAG_PUBLISH: &str = "github.event_name == 'push' && startsWith(github.ref, 'refs/tags/v')";

fn requires(release: &serde_json::Value, job: &str, prerequisite: &str) {
    assert!(
        release["jobs"][job]["needs"]
            .as_array()
            .unwrap_or_else(|| panic!("{job} must declare its prerequisites"))
            .contains(&serde_json::json!(prerequisite)),
        "{job} must require {prerequisite}"
    );
}

#[test]
fn release_publication_is_ordered_after_all_read_only_gates() {
    let release = workflow("release.yml");
    assert_eq!(
        release["jobs"]["real-kicad-acceptance"]["uses"],
        "./.github/workflows/e2e-kicad.yml"
    );
    requires(&release, "build", "validate-version");
    requires(&release, "pcm-package", "validate-version");
    for gate in [
        "validate-version",
        "real-kicad-acceptance",
        "build",
        "pcm-package",
        "linux-compatibility",
    ] {
        requires(&release, "prepare-publication", gate);
    }
    requires(&release, "attest", "prepare-publication");
    for gate in ["prepare-publication", "attest"] {
        requires(&release, "publish-npm", gate);
    }
    for gate in ["prepare-publication", "publish-npm"] {
        requires(&release, "release", gate);
    }
}

#[test]
fn release_dispatch_has_an_unprivileged_pcm_failure_exercise() {
    let release = workflow("release.yml");
    assert_eq!(
        release["on"]["workflow_dispatch"]["inputs"]["fail_pcm"]["type"],
        "boolean"
    );
    assert_eq!(release["permissions"], serde_json::json!({}));
    for job in ["attest", "publish-npm", "release"] {
        assert_eq!(release["jobs"][job]["if"], TAG_PUBLISH, "{job}");
    }
    let pcm_steps = release["jobs"]["pcm-package"]["steps"].as_array().unwrap();
    let injection = pcm_steps
        .iter()
        .position(|step| step["name"] == "Exercise a failed PCM lane")
        .unwrap();
    let upload = pcm_steps
        .iter()
        .position(|step| step["name"] == "Upload artifact")
        .unwrap();
    assert!(injection < upload);
    assert_eq!(
        pcm_steps[injection]["if"],
        "github.event_name == 'workflow_dispatch' && inputs.fail_pcm && matrix.platform == 'linux'"
    );
    let prepare = &release["jobs"]["prepare-publication"];
    assert_eq!(
        prepare["permissions"],
        serde_json::json!({"contents": "read"})
    );
    assert!(prepare["steps"].as_array().unwrap().iter().any(|step| {
        step["run"]
            .as_str()
            .is_some_and(|script| script.contains("npm pack --dry-run"))
    }));
}

#[test]
fn tag_publication_attests_and_uploads_the_checked_assets() {
    let release = workflow("release.yml");
    let step = |job: &str, name: &str| {
        release["jobs"][job]["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|step| step["name"] == name)
            .unwrap_or_else(|| panic!("{job} must contain {name}"))
    };
    for job in ["attest", "publish-npm", "release"] {
        assert_eq!(
            step(job, "Download verified release assets")["with"]["name"],
            "verified-release-assets"
        );
        assert!(step(job, "Check release asset digests")["run"]
            .as_str()
            .unwrap()
            .contains("sha256sum --check --strict SHA256SUMS"));
    }
    let attestation = step("attest", "Attest release archives");
    assert_eq!(
        attestation["with"]["subject-checksums"],
        "artifacts/SHA256SUMS"
    );
    assert_eq!(
        release["jobs"]["attest"]["permissions"]["id-token"],
        "write"
    );
    assert_eq!(
        release["jobs"]["attest"]["permissions"]["attestations"],
        "write"
    );
    let github = step("release", "Stage GitHub Release as a draft");
    assert_eq!(github["with"]["draft"], true);
    assert_eq!(github["with"]["overwrite_files"], false);
    let readback = step("release", "Verify uploaded draft release assets");
    let readback_script = readback["run"].as_str().unwrap();
    assert!(readback_script.contains("--json isDraft,tagName,assets"));
    assert!(readback_script.contains("--verify-draft artifacts"));
    let steps = release["jobs"]["release"]["steps"].as_array().unwrap();
    let positions = [
        github,
        readback,
        step("release", "Publish the complete GitHub Release"),
    ]
    .map(|step| {
        steps
            .iter()
            .position(|candidate| candidate == step)
            .unwrap()
    });
    assert!(positions[0] < positions[1] && positions[1] < positions[2]);
    assert_eq!(
        github["with"]["prerelease"],
        "${{ contains(github.ref_name, '-') }}"
    );
    let publish = step("release", "Publish the complete GitHub Release");
    assert_eq!(
        publish["env"]["PRERELEASE"],
        "${{ contains(github.ref_name, '-') }}"
    );
    assert_eq!(
        publish["env"]["LATEST"],
        "${{ !contains(github.ref_name, '-') }}"
    );
    assert!(publish["run"].as_str().unwrap().contains("--draft=false"));
    assert_eq!(github["with"]["fail_on_unmatched_files"], true);
    assert!(github["with"]["files"]
        .as_str()
        .unwrap()
        .contains("artifacts/SHA256SUMS"));
}

#[test]
fn workflow_permissions_and_external_action_revisions_are_explicit() {
    let workflow_dir = repository_root().join(".github/workflows");
    for entry in std::fs::read_dir(workflow_dir).unwrap() {
        let entry = entry.unwrap();
        if !matches!(
            entry
                .path()
                .extension()
                .and_then(|extension| extension.to_str()),
            Some("yml" | "yaml")
        ) {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_str().unwrap();
        let document = workflow(name);
        assert!(
            document["permissions"].is_object(),
            "{name} must declare permissions"
        );
        let jobs = document["jobs"].as_object().unwrap();
        for job in jobs.values() {
            let references = job.get("uses").into_iter().chain(
                job["steps"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|step| step.get("uses")),
            );
            for reference in references {
                let reference = reference.as_str().unwrap();
                if reference.starts_with("./") {
                    assert!(!reference.contains('@'), "{name}: {reference}");
                    continue;
                }
                let (_, revision) = reference
                    .rsplit_once('@')
                    .unwrap_or_else(|| panic!("{name}: missing revision on {reference}"));
                assert!(
                    revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
                    "{name}: {reference} must use a full commit SHA"
                );
            }
        }
    }
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
    requires(&release, "prepare-publication", "linux-compatibility");
    let smoke = &release["jobs"]["linux-compatibility"];
    assert_eq!(
        smoke["strategy"]["matrix"]["image"],
        serde_json::json!(["ubuntu:22.04", "debian:12"])
    );
    assert_eq!(
        smoke["strategy"]["matrix"]["os"],
        serde_json::json!(["ubuntu-22.04", "ubuntu-22.04-arm"])
    );
    requires(&release, "linux-compatibility", "validate-version");
    let execution = smoke["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|step| step["name"] == "Install candidate packages and initialize MCP")
        .unwrap();
    assert_eq!(
        execution["env"]["VERSION"],
        "${{ needs.validate-version.outputs.version }}"
    );
    let script = execution["run"].as_str().unwrap();
    assert!(script.contains(r#"npm/scripts/stage.js "$VERSION" artifacts"#));
    assert!(script.contains(r#"' bash "$VERSION""#));
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

#[test]
fn every_release_build_executes_its_binary_before_upload() {
    let release = workflow("release.yml");
    let build = &release["jobs"]["build"];
    assert_eq!(
        build["strategy"]["matrix"]["include"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    let steps = build["steps"].as_array().unwrap();
    let smoke = steps
        .iter()
        .position(|step| step["name"] == "Smoke test release binary")
        .unwrap();
    assert!(
        steps[smoke].get("if").is_none(),
        "every lane must smoke its binary"
    );
    let upload = steps
        .iter()
        .position(|step| step["name"] == "Upload artifact")
        .unwrap();
    assert!(smoke < upload);
}

#[test]
fn lint_and_dependency_gates_cover_platforms_and_both_lockfiles() {
    let ci = workflow("ci.yml");
    assert_eq!(ci["permissions"], serde_json::json!({"contents": "read"}));
    assert_eq!(ci["jobs"]["clippy"]["name"], "Clippy");
    assert_eq!(ci["jobs"]["clippy"]["needs"], "clippy-platform");
    let clippy = &ci["jobs"]["clippy-platform"];
    assert_eq!(
        clippy["strategy"]["matrix"]["os"],
        serde_json::json!(["ubuntu-latest", "windows-latest", "macos-latest"])
    );
    let viewer = ci["jobs"]["viewer"]["steps"].as_array().unwrap();
    for command in ["cargo fmt", "cargo clippy"] {
        assert!(viewer.iter().any(|step| step["run"]
            .as_str()
            .is_some_and(|script| script.contains(command))));
    }
    let steps = ci["jobs"]["dependency-policy"]["steps"].as_array().unwrap();
    for manifest in [
        serde_json::Value::Null,
        serde_json::json!("crates/schematic-viewer/Cargo.toml"),
    ] {
        assert!(steps
            .iter()
            .any(|step| step["with"]["command"] == "check bans sources"
                && step["with"]["manifest-path"] == manifest));
    }
}
