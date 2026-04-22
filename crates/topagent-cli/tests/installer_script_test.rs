use assert_cmd::Command;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

#[test]
fn test_install_script_installs_precompiled_release_asset_from_local_release_dir() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let repo_root = manifest_dir.parent().unwrap().parent().unwrap();
    let script = repo_root.join("scripts/install.sh");

    let root = TempDir::new().unwrap();
    let release_root = root.path().join("releases");
    let release_tag_dir = release_root.join("download").join("vtest");
    let install_root = root.path().join("install-root");
    let home_dir = root.path().join("home");

    fs::create_dir_all(&release_tag_dir).unwrap();
    fs::create_dir_all(&home_dir).unwrap();

    let compiled_bin = Command::cargo_bin("topagent")
        .unwrap()
        .get_program()
        .to_owned();
    let asset_name = "topagent-x86_64-unknown-linux-gnu";
    let asset_path = release_tag_dir.join(asset_name);
    fs::copy(compiled_bin, &asset_path).unwrap();

    let checksum_output = std::process::Command::new("sha256sum")
        .arg(asset_name)
        .current_dir(&release_tag_dir)
        .output()
        .unwrap();
    assert!(checksum_output.status.success());
    fs::write(
        release_tag_dir.join(format!("{asset_name}.sha256")),
        checksum_output.stdout,
    )
    .unwrap();

    let mut cmd = Command::new("bash");
    cmd.arg(script)
        .env("HOME", &home_dir)
        .env("PATH", "/usr/bin:/bin")
        .env("TOPAGENT_INSTALL_ROOT", &install_root)
        .env(
            "TOPAGENT_INSTALL_RELEASE_BASE_URL",
            format!("file://{}", release_root.display()),
        )
        .env("TOPAGENT_INSTALL_VERSION", "vtest")
        .env("TOPAGENT_SKIP_INSTALL", "1")
        .assert()
        .success();

    let installed_bin = install_root.join("bin").join("topagent");
    assert!(installed_bin.is_file());

    let mut help_cmd = Command::new(&installed_bin);
    help_cmd.arg("--help").assert().success();
}
