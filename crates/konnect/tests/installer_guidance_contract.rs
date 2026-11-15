use std::process::Command;

#[test]
fn bundled_skill_and_hook_content_are_available_without_installing() {
    for name in ["konnect", "kicad-schematic", "pre-pcb-ipc"] {
        let output = Command::new(env!("CARGO_BIN_EXE_konnect"))
            .args(["skill", name])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.stdout.is_empty());
        assert!(String::from_utf8(output.stdout).unwrap().contains("KiCad"));
    }
    let output = Command::new(env!("CARGO_BIN_EXE_konnect"))
        .args(["hook", "pre-pcb-ipc"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["hookSpecificOutput"]["hookEventName"], "PreToolUse");
    assert!(body["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap()
        .contains("KiCad"));
}

#[test]
fn unknown_guidance_names_exit_without_printing_instructions() {
    for command in ["skill", "hook"] {
        for name in ["", "unknown-guidance"] {
            let output = Command::new(env!("CARGO_BIN_EXE_konnect"))
                .args([command, name])
                .output()
                .unwrap();
            assert!(!output.status.success());
            assert!(output.stdout.is_empty());
            assert!(!output.stderr.is_empty());
        }
    }
}
