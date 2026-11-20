//! Differential tests for the always-allow argv denylist — port of Go
//! `TestArgvAllowDenylist` + pure helper behavior.

use artex::intercept::argv_allow::{argv_allow_denied, argv_program_name, check_argv_allow};

#[test]
fn argv_program_name_normalization() {
    assert_eq!(argv_program_name("/usr/bin/python3.11"), "python");
    assert_eq!(argv_program_name("PHP8.2"), "php");
    assert_eq!(argv_program_name("perl5"), "perl");
    assert_eq!(argv_program_name("pip3"), "pip");
    assert_eq!(argv_program_name("mkfs.ext4"), "mkfs");
    assert_eq!(argv_program_name("/sbin/mkfs.xfs"), "mkfs");
    assert_eq!(argv_program_name("."), ".");
    assert_eq!(argv_program_name("7z"), "7z", "all-digit name kept");
}

#[test]
fn denylist_cases() {
    for prog in [
        "python",
        "python3",
        "python3.11",
        "/usr/bin/python3.11",
        "php8.2",
        "ruby3.2",
        "perl5",
        "pip3",
        "env",
        "/usr/bin/env",
        "bash",
        "/bin/sh",
        "find",
        "rm",
        "mkfs",
        "mkfs.ext4",
        "/sbin/mkfs.xfs",
        "docker",
        "kubectl",
        "npm",
        "sudo",
        "xargs",
        "Rscript",
        ".",
    ] {
        assert!(argv_allow_denied(prog), "{prog} should be denied");
    }
    for prog in [
        "nmap",
        "git",
        "/usr/bin/git",
        "curl",
        "ls",
        "gh",
        "x",
        "7z",
        "pythonic",
    ] {
        assert!(!argv_allow_denied(prog), "{prog} should be allowed");
    }
}

#[test]
fn check_argv_allow_cases() {
    assert!(check_argv_allow(&[]).is_ok());
    assert!(check_argv_allow(&["ls".to_string(), "-l".to_string()]).is_ok());
    let err = check_argv_allow(&["python3".to_string()]).unwrap_err();
    assert!(err.to_string().contains("cannot be always allowed"));
}
