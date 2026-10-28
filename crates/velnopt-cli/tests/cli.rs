//! End-to-end tests of the `velnopt` executable's command-line behavior.

use std::{path::PathBuf, process::Command};

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_velnopt"))
}

#[test]
fn validates_example_without_a_display_server() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/panel.toml");
    let output = command()
        .arg("check")
        .arg(path)
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .output()
        .expect("CLI should start");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("configuration is valid"));
    assert!(output.stderr.is_empty());
}

#[test]
fn validates_live_widgets_without_hyprland_ipc() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/live-hyprland.toml");
    let output = command()
        .arg("check")
        .arg(path)
        .env_remove("HYPRLAND_INSTANCE_SIGNATURE")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .expect("CLI should start");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
}

#[test]
fn reports_missing_files_without_panicking() {
    // a file can't also be a directory, so this path can never exist
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml/missing.toml");
    let output = command()
        .arg("check")
        .arg(path)
        .output()
        .expect("CLI should start");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("missing.toml"));
    assert!(!stderr.contains("panicked"));
}

#[test]
fn reports_invalid_configuration() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let output = command()
        .arg("check")
        .arg(path)
        .output()
        .expect("CLI should start");
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown field"));
}

#[test]
fn provides_help_and_version() {
    for argument in ["--help", "--version"] {
        let output = command().arg(argument).output().expect("CLI should start");
        assert!(output.status.success());
        assert!(!output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        let stdout = String::from_utf8_lossy(&output.stdout);
        if argument == "--version" {
            assert_eq!(
                stdout.trim(),
                concat!("velnopt ", env!("CARGO_PKG_VERSION"))
            );
        } else {
            assert!(stdout.contains("Usage: velnopt <COMMAND>"));
            assert!(!stdout.contains("velnopt-cli"));
        }
    }
}

#[test]
fn rejects_missing_and_unknown_arguments() {
    for arguments in [vec![], vec!["check"], vec!["run"], vec!["--unknown"]] {
        let output = command()
            .args(arguments)
            .output()
            .expect("CLI should start");
        assert_eq!(output.status.code(), Some(2));
        assert!(!output.stderr.is_empty());
    }
}

#[test]
fn validates_run_configuration_before_connecting_to_a_display() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml");
    let output = command()
        .arg("run")
        .arg(path)
        .env("WAYLAND_DISPLAY", "/nonexistent/velnopt-test-socket")
        .output()
        .expect("CLI should start");
    assert_eq!(output.status.code(), Some(1));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("unknown field"));
    assert!(!error.contains("cannot connect"));
}

#[test]
fn reports_absent_hyprland_session() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/panel.toml");
    let output = command()
        .arg("run")
        .arg(path)
        .env_remove("WAYLAND_SOCKET")
        .env("WAYLAND_DISPLAY", "/nonexistent/velnopt-test-socket")
        .output()
        .expect("CLI should start");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("run inside your Hyprland session"));
}

#[test]
fn rejects_nonregular_configuration_files() {
    let output = command()
        .args(["check", "/dev/null"])
        .output()
        .expect("CLI should start");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("regular file"));
}
