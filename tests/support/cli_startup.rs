//! Exercise the real binary on a small stack without aborting the test runner.

use std::process::{Command, Output};

#[cfg(target_os = "linux")]
fn constrain_child_stack(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    // Only async-signal-safe syscalls run between fork and exec. The parent and
    // other tests keep their existing limits; a regression aborts only this CLI.
    unsafe {
        command.pre_exec(|| {
            let no_core = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::setrlimit(libc::RLIMIT_CORE, &no_core) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let stack = libc::rlimit {
                rlim_cur: 1024 * 1024,
                rlim_max: 1024 * 1024,
            };
            if libc::setrlimit(libc::RLIMIT_STACK, &stack) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn constrain_child_stack(_command: &mut Command) {
    // Windows keeps the actual executable's native main-thread stack budget.
}

fn startup_output(root: &std::path::Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_remem"));
    command
        .args(args)
        .env("HOME", root.join("home"))
        .env("USERPROFILE", root.join("home"))
        .env("REMEM_DATA_DIR", root.join("data"))
        .env("NO_COLOR", "1")
        .env_remove("CODEX_HOME")
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("REMEM_CONFIG")
        .env_remove("REMEM_STDERR_TO_LOG")
        .env_remove("REMEM_DEBUG");
    constrain_child_stack(&mut command);
    command
        .output()
        .expect("execute isolated CLI startup probe")
}

#[test]
fn cli_version_and_help_fit_the_native_main_thread_stack() {
    let root = super::install_status_temp_root();
    let home = root.join("home");
    std::fs::create_dir_all(&home).expect("create isolated startup home");
    let cases: &[&[&str]] = &[
        &["--version"],
        &["--help"],
        &["context", "--help"],
        &["govern", "--help"],
        &["reroute", "--help"],
        &["search", "--help"],
        &["install", "--help"],
        &["doctor", "--help"],
        &["worker", "--help"],
    ];
    for args in cases {
        let output = startup_output(&root, args);
        assert!(
            output.status.success(),
            "CLI {args:?} failed on the child stack budget: {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        if args.len() == 1 && args[0] == "--version" {
            assert!(
                stdout.starts_with("remem ") && stdout.contains("schema v"),
                "{stdout}"
            );
        } else {
            assert!(
                stdout.contains("Usage:"),
                "missing help for {args:?}: {stdout}"
            );
        }
    }
    assert!(
        !root.join("data").exists(),
        "help/version must not create a store"
    );
    assert_eq!(
        std::fs::read_dir(&home).unwrap().count(),
        0,
        "help/version wrote host configuration"
    );
    std::fs::remove_dir_all(root).expect("remove test-owned startup fixture");
}
