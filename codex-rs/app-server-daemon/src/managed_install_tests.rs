use pretty_assertions::assert_eq;

use super::ExecutableIdentity;
use super::executable_identity;
use super::parse_codex_version;

#[test]
fn parses_codex_cli_version_output() {
    assert_eq!(
        parse_codex_version("codex 1.2.3\n").expect("version"),
        "1.2.3"
    );
}

#[test]
fn rejects_malformed_codex_cli_version_output() {
    assert!(parse_codex_version("codex\n").is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn managed_version_probe_bounds_output_and_reaps_a_stalled_child() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let executable = directory.path().join("codex");
    codex_utils_cargo_bin::write_executable(&executable, "#!/bin/sh\nprintf 'codex 1.2.3\\n'\n")
        .expect("valid version helper");
    assert_eq!(
        super::managed_codex_version(&executable)
            .await
            .expect("version"),
        "1.2.3"
    );

    codex_utils_cargo_bin::write_executable(
        &executable,
        "#!/bin/sh\nwhile :; do printf 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; done\n",
    )
    .expect("oversized version helper");
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        super::managed_codex_version(&executable),
    )
    .await
    .expect("output bound must finish")
    .expect_err("output too large");
    assert!(error.to_string().contains("exceeded 4096 bytes"));

    codex_utils_cargo_bin::write_executable(
        &executable,
        "#!/bin/sh\nprintf '%s' \"$$\" > \"$0.pid\"\nexec sleep 60\n",
    )
    .expect("stalled version helper");
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(8),
        super::managed_codex_version(&executable),
    )
    .await
    .expect("version probe must enforce its deadline")
    .expect_err("stalled helper");
    assert!(error.to_string().contains("timed out after 5 seconds"));
    let pid: libc::pid_t = std::fs::read_to_string(executable.with_extension("pid"))
        .expect("helper pid")
        .parse()
        .expect("pid number");
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "timed-out child must be reaped"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

#[cfg(windows)]
fn windows_version_probe_command(script: &str) -> tokio::process::Command {
    let powershell =
        std::path::PathBuf::from(std::env::var_os("SystemRoot").expect("Windows root"))
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    assert!(
        powershell.is_file(),
        "Windows PowerShell fixture executable"
    );
    let mut command = tokio::process::Command::new(powershell);
    command.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        script,
    ]);
    command
}

#[cfg(windows)]
#[tokio::test]
async fn managed_version_probe_windows_preserves_versions_and_classifies_failures() {
    let version = super::managed_codex_version_from_command(windows_version_probe_command(
        r#"[Console]::Out.Write("codex 1.2.3`n")"#,
    ))
    .await
    .expect("valid native version probe");
    assert_eq!(version, "1.2.3");

    let error = super::managed_codex_version_from_command(windows_version_probe_command("exit 23"))
        .await
        .expect_err("unsuccessful native version probe");
    assert!(error.to_string().contains("exited with status"));

    let error = super::managed_codex_version_from_command(windows_version_probe_command(
        r#"[Console]::Out.Write("unexpected")"#,
    ))
    .await
    .expect_err("malformed native version probe");
    assert!(error.to_string().contains("version"));
}

#[cfg(windows)]
#[tokio::test]
async fn managed_version_probe_windows_bounds_output_and_stops_children() {
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::io::FromRawHandle;
    use std::os::windows::io::OwnedHandle;
    use std::time::Duration;
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
    use windows_sys::Win32::System::Threading::OpenProcess;
    use windows_sys::Win32::System::Threading::PROCESS_SYNCHRONIZE;
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    let directory = tempfile::tempdir().expect("temporary directory");
    for (mode, expected_error) in [
        ("overflow", "exceeded 4096 bytes"),
        ("stall", "timed out after 5 seconds"),
    ] {
        let pid_file = directory.path().join(format!("{mode}.pid"));
        let release_file = directory.path().join(format!("{mode}.release"));
        let mut command = windows_version_probe_command(
            r#"
$ErrorActionPreference = 'Stop'
if ([Console]::In.Read() -ne -1) { exit 19 }
$pidFile = $env:CODEX_TEST_VERSION_PROBE_PID
[IO.File]::WriteAllText($pidFile + '.tmp', [string]$PID)
[IO.File]::Move($pidFile + '.tmp', $pidFile)
while (-not [IO.File]::Exists($env:CODEX_TEST_VERSION_PROBE_RELEASE)) {
    Start-Sleep -Milliseconds 10
}
if ($env:CODEX_TEST_VERSION_PROBE_MODE -eq 'overflow') {
    while ($true) {
        [Console]::Out.Write('x' * 8192)
        [Console]::Out.Flush()
    }
} else {
    Start-Sleep -Seconds 60
}
"#,
        );
        command
            .env("CODEX_TEST_VERSION_PROBE_PID", &pid_file)
            .env("CODEX_TEST_VERSION_PROBE_RELEASE", &release_file)
            .env("CODEX_TEST_VERSION_PROBE_MODE", mode);

        let (result, process) = tokio::time::timeout(Duration::from_secs(8), async {
            let mut probe = Box::pin(super::managed_codex_version_from_command(command));
            let ready = async {
                loop {
                    match tokio::fs::read_to_string(&pid_file).await {
                        Ok(pid) => {
                            let pid: u32 = pid.parse().expect("published child PID");
                            // Hold the process object before releasing the fixture;
                            // a later PID lookup could observe a different process.
                            let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
                            assert!(!handle.is_null(), "open native child process");
                            break unsafe { OwnedHandle::from_raw_handle(handle) };
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                        Err(error) => panic!("read child PID: {error}"),
                    }
                }
            };
            let process = tokio::select! {
                result = &mut probe => panic!("probe completed before fixture was ready: {result:?}"),
                process = ready => process,
            };
            assert_eq!(
                unsafe { WaitForSingleObject(process.as_raw_handle(), 0) },
                WAIT_TIMEOUT,
                "fixture must be running before testing cleanup"
            );
            tokio::fs::write(&release_file, [])
                .await
                .expect("release native fixture");
            (probe.await, process)
        })
        .await
        .expect("native probe must finish within its bounded completion budget");

        let stopped = unsafe { WaitForSingleObject(process.as_raw_handle(), 0) };
        let error = result.expect_err("invalid native version probe");
        assert!(error.to_string().contains(expected_error), "{error:#}");
        assert_eq!(
            stopped, WAIT_OBJECT_0,
            "native child must stop before return"
        );

        assert_eq!(
            super::managed_codex_version_from_command(windows_version_probe_command(
                r#"[Console]::Out.Write("codex 1.2.3`n")"#,
            ))
            .await
            .expect("the next native probe should succeed"),
            "1.2.3"
        );
    }
}

#[tokio::test]
async fn executable_identity_uses_path_and_binary_contents() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let executable = directory.path().join("codex");
    // Span multiple reads, including a partial final buffer, and preserve the
    // digest stored by older clients that hashed the complete file in memory.
    let mut bytes: Vec<u8> = (0..200_003).map(|index| (index % 251) as u8).collect();
    for contents in [&bytes[..], &[][..]] {
        std::fs::write(&executable, contents).expect("write executable");
        assert_eq!(
            executable_identity(&executable).await.expect("identity"),
            ExecutableIdentity {
                digest: *blake3::hash(contents).as_bytes(),
                path_digest: Some(super::path_digest(
                    &std::fs::canonicalize(&executable).expect("canonical executable"),
                )),
            }
        );
    }
    let copy = directory.path().join("codex-copy");
    std::fs::copy(&executable, &copy).expect("copy executable");
    let identity = executable_identity(&executable).await.expect("identity");
    let copy_identity = executable_identity(&copy).await.expect("copy identity");
    assert_ne!(identity, copy_identity);
    assert!(identity.same_contents(&copy_identity));
    std::fs::write(&executable, &bytes).expect("write executable");
    let old = executable_identity(&executable).await.expect("identity");
    bytes[100_000] ^= 1;
    std::fs::write(&executable, bytes).expect("replace executable");
    assert_ne!(
        executable_identity(&executable)
            .await
            .expect("new identity"),
        old
    );
}
