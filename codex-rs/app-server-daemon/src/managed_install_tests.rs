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
