//! Criterion microbenchmarks for abox-core hot paths.
//!
//! Run with:
//!     cargo bench -p abox-core
//!
//! These cover the CPU-bound code that runs on every proxied request
//! (policy evaluation, request/response serialization, boot meta
//! generation). They don't need a VM or /dev/kvm — they run in CI
//! and catch performance regressions in the policy engine, the
//! serialization layer, and the boot meta stager.

use criterion::{black_box, criterion_group, criterion_main, Criterion};

// ─── Policy evaluation ──────────────────────────────────────────────────────

fn policy_evaluation(c: &mut Criterion) {
    use abox_core::policy::{CliPolicy, PolicyEngine, PolicyFile};

    let policy = PolicyFile {
        cli: vec![
            CliPolicy {
                command: "git".to_string(),
                allow: vec![
                    r"^(status|log|diff|show|branch)".to_string(),
                    r"^push\s+origin\s+\S+$".to_string(),
                    r"^pull\s+".to_string(),
                    r"^fetch\s+".to_string(),
                    r"^add\s+".to_string(),
                    r"^commit\s+".to_string(),
                    r"^checkout\s+".to_string(),
                ],
                deny: vec![
                    r"--force".to_string(),
                    r"-f\b".to_string(),
                    r"push\s+--delete".to_string(),
                ],
                forward_ssh_agent: true,
            },
            CliPolicy {
                command: "aws".to_string(),
                allow: vec![r"^s3\s+(ls|cp|sync)".to_string()],
                deny: vec![r"^iam\s+".to_string()],
                forward_ssh_agent: false,
            },
        ],
        egress: vec![],
        default_cli_action: "deny".to_string(),
        default_egress_action: "deny".to_string(),
        bypass_tls: vec![],
    };

    let engine = PolicyEngine::from_policy_file(policy).unwrap();

    let mut group = c.benchmark_group("policy_evaluate_cli");

    // Allowed: simple git status (fast path — first allow pattern matches).
    let args_status: Vec<String> = vec!["status".into()];
    group.bench_function("git_status_allowed", |b| {
        b.iter(|| engine.evaluate_cli(black_box("git"), black_box(&args_status)));
    });

    // Allowed: git push origin main (matches the push allow pattern after
    // scanning past the shorter patterns).
    let args_push: Vec<String> = vec!["push".into(), "origin".into(), "main".into()];
    group.bench_function("git_push_allowed", |b| {
        b.iter(|| engine.evaluate_cli(black_box("git"), black_box(&args_push)));
    });

    // Denied: git push --force (hits the deny pattern).
    let args_force: Vec<String> =
        vec!["push".into(), "--force".into(), "origin".into(), "main".into()];
    group.bench_function("git_push_force_denied", |b| {
        b.iter(|| engine.evaluate_cli(black_box("git"), black_box(&args_force)));
    });

    // Denied via global-option stripping: git -c foo=bar push --force.
    let args_bypass: Vec<String> = vec![
        "-c".into(),
        "core.hooks=./evil".into(),
        "push".into(),
        "--force".into(),
        "origin".into(),
        "main".into(),
    ];
    group.bench_function("git_dash_c_push_force_denied", |b| {
        b.iter(|| engine.evaluate_cli(black_box("git"), black_box(&args_bypass)));
    });

    // Unknown command: default deny path.
    let args_rm: Vec<String> = vec!["-rf".into(), "/".into()];
    group.bench_function("unknown_cmd_denied", |b| {
        b.iter(|| engine.evaluate_cli(black_box("rm"), black_box(&args_rm)));
    });

    group.finish();
}

// ─── Serialization ──────────────────────────────────────────────────────────

fn serialization(c: &mut Criterion) {
    use abox_protocol::{ProxyRequest, ProxyResponse};

    let request = ProxyRequest {
        command: "git".to_string(),
        args: vec!["push".into(), "origin".into(), "main".into()],
        cwd: "/workspace".to_string(),
        sandbox_id: Some("fix-auth".to_string()),
    };
    let request_json = serde_json::to_string(&request).unwrap();

    let response = ProxyResponse::from_exit(
        0,
        "On branch main\nnothing to commit, working tree clean\n".to_string(),
        String::new(),
    );
    let response_json = serde_json::to_string(&response).unwrap();

    let mut group = c.benchmark_group("proxy_serialization");

    group.bench_function("request_serialize", |b| {
        b.iter(|| serde_json::to_string(black_box(&request)).unwrap());
    });

    group.bench_function("request_deserialize", |b| {
        b.iter(|| serde_json::from_str::<ProxyRequest>(black_box(&request_json)).unwrap());
    });

    group.bench_function("response_serialize", |b| {
        b.iter(|| serde_json::to_string(black_box(&response)).unwrap());
    });

    group.bench_function("response_deserialize", |b| {
        b.iter(|| serde_json::from_str::<ProxyResponse>(black_box(&response_json)).unwrap());
    });

    group.finish();
}

// ─── Boot meta generation ───────────────────────────────────────────────────

fn boot_meta(c: &mut Criterion) {
    use abox_core::boot_meta::BootMeta;

    let meta = BootMeta {
        sandbox_id: "fix-auth".into(),
        agent_command: vec!["claude".into(), "--model".into(), "opus".into()],
        env: vec![
            ("ANTHROPIC_API_KEY".into(), "sk-ant-12345".into()),
            ("PATH".into(), "/usr/local/bin:/usr/bin:/bin".into()),
        ],
        credential_files: vec![],
        mount_excludes: vec![],
    };

    let mut group = c.benchmark_group("boot_meta");

    group.bench_function("to_json", |b| {
        b.iter(|| black_box(&meta).to_json().unwrap());
    });

    group.bench_function("runner_script", |b| {
        b.iter(|| black_box(&meta).runner_script());
    });

    let json = meta.to_json().unwrap();
    group.bench_function("from_json", |b| {
        b.iter(|| BootMeta::from_json(black_box(&json)).unwrap());
    });

    group.bench_function("stage_to_tmpdir", |b| {
        let tmp = tempfile::tempdir().unwrap();
        b.iter(|| {
            black_box(&meta).stage(tmp.path()).unwrap();
        });
    });

    group.finish();
}

criterion_group!(benches, policy_evaluation, serialization, boot_meta);
criterion_main!(benches);
