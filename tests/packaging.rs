//! Checks that the shipped packaging agrees with the documented install.
//!
//! These are not code paths, so nothing else would catch them drifting apart —
//! and the failure mode is a service that refuses to start, or a binary that is
//! installed somewhere the user's `PATH` never looks.

use std::fs;

/// Where the README tells the user to install, and where the unit looks.
const INSTALL_ROOT: &str = "~/.local";

#[test]
fn the_service_runs_the_binary_the_readme_installs() {
    let readme = fs::read_to_string("README.md").expect("README.md");
    let service = fs::read_to_string("packaging/ledmat.service").expect("the unit");

    assert!(
        readme.contains(&format!("cargo install --path . --root {INSTALL_ROOT}")),
        "the README no longer installs into {INSTALL_ROOT}"
    );

    // `--root ~/.local` puts the binary in `~/.local/bin`; systemd expands %h to
    // the user's home. A plain `cargo install --path .` would land it in
    // ~/.cargo/bin instead, which the unit would then fail to execute.
    let expected = format!("ExecStart={}/bin/ledmat", INSTALL_ROOT.replace('~', "%h"));
    assert!(
        service.contains(&expected),
        "the unit does not run the installed binary; expected {expected}"
    );
}

#[test]
fn the_udev_rule_sorts_before_the_one_that_grants_access() {
    // 73-seat-late.rules is what turns the uaccess tag into an ACL, and udev
    // runs rule files in lexical order. A rule numbered above that sets the tag
    // after the only rule that reads it: the symlinks appear, with no
    // permission behind them, and nothing says why.
    let rule = fs::read_dir("packaging")
        .expect("packaging/")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .find(|name| {
            std::path::Path::new(name)
                .extension()
                .is_some_and(|e| e == "rules")
        })
        .expect("a udev rule");

    let prefix: u32 = rule
        .split('-')
        .next()
        .and_then(|number| number.parse().ok())
        .unwrap_or_else(|| panic!("{rule} does not start with a sort order"));

    assert!(prefix < 73, "{rule} is applied too late to grant access");
}

#[test]
fn the_service_gives_the_panels_time_to_go_dark() {
    // Shutdown clears the panels over a serial link that only drains about 60
    // commands a second. Killing the process too eagerly leaves the LEDs lit.
    let service = fs::read_to_string("packaging/ledmat.service").expect("the unit");
    assert!(
        service.contains("KillSignal=SIGTERM"),
        "clearing needs SIGTERM"
    );
    assert!(
        service.contains("TimeoutStopSec="),
        "no time to clear the panels"
    );
}
