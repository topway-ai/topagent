use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

fn read_repo_file(path: &str) -> String {
    let path = repo_root().join(path);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()))
}

#[test]
fn release_runs_full_quality_gate_before_packaging_or_publishing() {
    let release = read_repo_file(".github/workflows/release.yml");
    let ci = read_repo_file(".github/workflows/ci.yml");
    let gate = read_repo_file("scripts/ci-gate.sh");

    assert!(
        ci.contains("scripts/ci-gate.sh"),
        "CI must use the shared quality gate"
    );
    assert!(
        release.contains("scripts/ci-gate.sh --release-binary"),
        "release must run the shared quality gate with the release binary build"
    );

    let gate_idx = release
        .find("scripts/ci-gate.sh --release-binary")
        .expect("release quality gate step missing");
    let package_idx = release
        .find("Package release assets")
        .expect("release package step missing");
    let publish_idx = release
        .find("Publish GitHub release")
        .expect("release publish step missing");
    assert!(
        gate_idx < package_idx && gate_idx < publish_idx,
        "release quality gate must run before packaging and publishing"
    );

    for required in [
        "cargo fmt --all --check",
        "cargo clippy --all-targets -- -D warnings",
        "cargo test --locked",
        "cargo build --locked --release -p topagent-cli --bin topagent",
    ] {
        assert!(
            gate.contains(required),
            "shared quality gate is missing required command: {required}"
        );
    }
}
