use serde_json::Value;
use std::collections::HashSet;
use std::process::Command;

#[test]
fn capabilities_flag_prints_envelope() {
    let output = Command::new(env!("CARGO_BIN_EXE_abox"))
        .arg("--capabilities")
        .output()
        .expect("failed to spawn abox --capabilities");

    assert!(
        output.status.success(),
        "abox --capabilities exited non-zero: stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    let json: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON: {e}; stdout={stdout}"));

    let obj = json.as_object().expect("top-level JSON is an object");

    let versions = obj
        .get("protocolVersions")
        .and_then(Value::as_array)
        .expect("protocolVersions is an array");
    assert!(!versions.is_empty(), "protocolVersions is empty");
    assert!(versions.iter().all(Value::is_number), "protocolVersions entries must be numbers");
    let version_numbers: Vec<i64> = versions.iter().filter_map(Value::as_i64).collect();
    assert_eq!(version_numbers, vec![1, 3], "protocolVersions must equal [1, 3] per Appendix A.1");

    let kinds = obj.get("taskKinds").and_then(Value::as_array).expect("taskKinds is an array");
    let kind_strs: Vec<&str> = kinds.iter().filter_map(Value::as_str).collect();
    assert_eq!(
        kind_strs,
        vec!["assistant_job", "explicit_command", "verification_check"],
        "taskKinds must match Appendix A.1 exactly",
    );

    let engines = obj
        .get("executionEngines")
        .and_then(Value::as_array)
        .expect("executionEngines is an array");
    let engine_strs: Vec<&str> = engines.iter().filter_map(Value::as_str).collect();
    assert_eq!(
        engine_strs,
        vec!["agent_cli", "shell"],
        "executionEngines must match Appendix A.1 exactly",
    );

    let allowed_keys: HashSet<&str> =
        ["protocolVersions", "taskKinds", "executionEngines"].into_iter().collect();
    for key in obj.keys() {
        assert!(
            allowed_keys.contains(key.as_str()),
            "unexpected key in --capabilities envelope: {key}",
        );
    }
}

#[test]
fn capabilities_flag_bypasses_config_load() {
    let output = Command::new(env!("CARGO_BIN_EXE_abox"))
        .arg("--config")
        .arg("/nonexistent/path/that/does/not/exist/abox.toml")
        .arg("--capabilities")
        .output()
        .expect("failed to spawn abox --capabilities with bad config");

    assert!(
        output.status.success(),
        "abox --capabilities must bypass config load; exited non-zero with stderr={}",
        String::from_utf8_lossy(&output.stderr),
    );
}
