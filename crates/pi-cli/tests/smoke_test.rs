use assert_cmd::Command;

#[test]
fn test_cli_smoke_help() {
    let mut cmd = Command::cargo_bin("pi").unwrap();
    cmd.arg("--help").assert().success();
}

#[test]
fn test_cli_smoke_instruction() {
    let mut cmd = Command::cargo_bin("pi").unwrap();
    cmd.args(["run", "say hello"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Understood"));
}
