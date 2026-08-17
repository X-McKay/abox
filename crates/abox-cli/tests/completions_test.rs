use std::process::Command;

#[test]
fn completions_bypass_config_load() {
    let output = Command::new(env!("CARGO_BIN_EXE_abox"))
        .args([
            "--repo",
            "/nonexistent/path/that/is/not/a/repository",
            "--config",
            "/nonexistent/path/that/does/not/exist/abox.toml",
            "completions",
            "bash",
        ])
        .output()
        .expect("failed to spawn abox completions bash");

    assert!(
        output.status.success(),
        "abox completions must bypass config load; stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("completion script is UTF-8");
    assert!(
        stdout.contains("_abox()") && stdout.contains("complete -F _abox"),
        "unexpected bash completion output"
    );
}

#[test]
fn completions_support_each_documented_shell() {
    for shell in ["bash", "elvish", "fish", "powershell", "zsh"] {
        let output = Command::new(env!("CARGO_BIN_EXE_abox"))
            .args(["completions", shell])
            .output()
            .unwrap_or_else(|error| panic!("failed to spawn abox completions {shell}: {error}"));

        assert!(
            output.status.success(),
            "abox completions {shell} exited non-zero: {}",
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(!output.stdout.is_empty(), "abox completions {shell} produced no output");
    }
}
