//! Clipboard routing, native ownership, and payload-boundary regressions.

use super::ClipboardLease;
use super::CopyEnvironment;
use super::CopyFormat;
use super::CopyOutcome;
use super::CopyStatus;
use super::OSC52_MAX_RAW_BYTES;
use super::copy_to_clipboard_with;
use pretty_assertions::assert_eq;
use std::cell::RefCell;

#[test]
fn local_tmux_preserves_native_html_when_terminal_silently_rejects_copy() {
    let calls = RefCell::new(Vec::new());
    let result = copy_to_clipboard_with(
        "**hello**",
        CopyFormat::Markdown,
        CopyEnvironment {
            ssh_session: false,
            tmux_session: true,
            wsl_session: false,
        },
        |_| {
            // Like default xterm: the command succeeds without delivery.
            calls.borrow_mut().push("tmux");
            Ok(())
        },
        |_| panic!("tmux send succeeded"),
        |text, html| {
            assert_eq!(
                (text, html),
                ("**hello**", Some("<p><strong>hello</strong></p>\n"))
            );
            calls.borrow_mut().push("native");
            Ok(Some(ClipboardLease::test()))
        },
        |_| panic!("native copy succeeded"),
    );
    assert!(matches!(result, Ok(CopyOutcome::Copied(Some(_)))));
    assert_eq!(calls.into_inner(), vec!["native", "tmux"]);
}

#[test]
fn copy_uses_osc52_when_tmux_fails_even_if_native_copy_succeeds() {
    let calls = RefCell::new(Vec::new());
    let result = copy_to_clipboard_with(
        "hello",
        CopyFormat::PlainText,
        CopyEnvironment {
            ssh_session: false,
            tmux_session: true,
            wsl_session: false,
        },
        |_| {
            calls.borrow_mut().push("tmux");
            Err("tmux unavailable".into())
        },
        |_| {
            calls.borrow_mut().push("osc52");
            Ok(())
        },
        |text, html| {
            assert_eq!((text, html), ("hello", None));
            calls.borrow_mut().push("native");
            Ok(Some(ClipboardLease::test()))
        },
        |_| panic!("native copy succeeded"),
    );
    assert!(matches!(result, Ok(CopyOutcome::Copied(Some(_)))));
    assert_eq!(calls.into_inner(), vec!["native", "tmux", "osc52"]);
}

fn local_tmux_environment() -> CopyEnvironment {
    CopyEnvironment {
        tmux_session: true,
        ssh_session: false,
        wsl_session: false,
    }
}

#[test]
fn oversized_tmux_copy_preserves_native_copy() {
    for (size, expected_calls) in [
        (OSC52_MAX_RAW_BYTES, vec!["native", "tmux"]),
        (OSC52_MAX_RAW_BYTES + 1, vec!["native", "osc52"]),
    ] {
        let text = "x".repeat(size);
        let calls = RefCell::new(Vec::new());
        let result = copy_to_clipboard_with(
            &text,
            CopyFormat::PlainText,
            local_tmux_environment(),
            |actual| {
                assert_eq!(actual, text);
                calls.borrow_mut().push("tmux");
                Ok(())
            },
            |_| {
                calls.borrow_mut().push("osc52");
                Err("payload too large".into())
            },
            |actual, html| {
                assert_eq!((actual, html), (text.as_str(), None));
                calls.borrow_mut().push("native");
                Ok(Some(ClipboardLease::test()))
            },
            |_| panic!("PowerShell fallback should not be needed"),
        );
        assert!(matches!(result, Ok(CopyOutcome::Copied(Some(_)))));
        assert_eq!(calls.into_inner(), expected_calls);
    }
}

#[test]
fn empty_copy_reports_failure_without_touching_clipboards() {
    let result = copy_to_clipboard_with(
        "",
        CopyFormat::PlainText,
        local_tmux_environment(),
        |_| panic!("empty input must not reach tmux"),
        |_| panic!("empty input must not reach OSC 52"),
        |_, _| panic!("empty input must not reach the native clipboard"),
        |_| panic!("empty input must not reach PowerShell"),
    );
    insta::assert_snapshot!(result.err().expect("empty selection should fail"));
}

#[test]
fn confirmed_copy_without_new_lease_preserves_native_ownership() {
    let mut lease = Some(ClipboardLease::test());

    assert_eq!(
        CopyOutcome::Copied(None).store(&mut lease),
        CopyStatus::Confirmed
    );
    assert!(lease.is_some());
}

#[test]
fn wsl_copy_skips_linux_display_and_preserves_remote_forwarding() {
    for tmux in [false, true] {
        let calls = RefCell::new(Vec::new());
        let text = "  café\n\t$literal `text` 🦀\n";
        let result = copy_to_clipboard_with(
            text,
            CopyFormat::Markdown,
            CopyEnvironment {
                ssh_session: true,
                wsl_session: true,
                tmux_session: tmux,
            },
            |actual| {
                assert_eq!(actual, text);
                calls.borrow_mut().push("tmux");
                Ok(())
            },
            |actual| {
                assert_eq!(actual, text);
                calls.borrow_mut().push("terminal");
                Ok(())
            },
            |_, _| panic!("WSLg initialization may block; WSL must never enter it"),
            |actual| {
                assert_eq!(actual, text);
                calls.borrow_mut().push("windows");
                Ok(())
            },
        );
        assert!(matches!(result, Ok(CopyOutcome::Copied(None))));
        assert_eq!(
            calls.into_inner(),
            vec!["windows", if tmux { "tmux" } else { "terminal" }]
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn windows_writer_preserves_large_unicode_text_and_rejects_expired_work() {
    use std::time::Duration;
    use std::time::Instant;
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("text");
    let command = || {
        let mut command = tokio::process::Command::new("sh");
        command
            .args(["-c", "cat > \"$1\"", "clipboard-test"])
            .arg(&output);
        command
    };
    let text = "  café\r\n\t$literal `text` 🦀\n".repeat(/*n*/ 8192);
    assert_eq!(
        super::write_clipboard_command(command(), &text, Instant::now()).await,
        Err("clipboard write timed out".into())
    );
    assert!(!output.exists());
    assert_eq!(
        super::write_clipboard_command(
            command(),
            &text,
            Instant::now() + Duration::from_secs(/*secs*/ 5)
        )
        .await,
        Ok(())
    );
    assert_eq!(std::fs::read_to_string(output).unwrap(), text);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn stalled_windows_writer_is_reaped_and_next_copy_can_finish() {
    use std::time::Duration;
    use std::time::Instant;
    for script in [
        "printf %s \"$$\" > \"$1\"; exec sleep 60",
        "printf %s \"$$\" > \"$1\"; cat >/dev/null; exec sleep 60",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("pid");
        let mut command = tokio::process::Command::new("sh");
        command
            .args(["-c", script, "clipboard-test"])
            .arg(&pid_file);
        let text = "x".repeat(/*n*/ 1024 * 1024);
        assert_eq!(
            super::write_clipboard_command(
                command,
                &text,
                Instant::now() + Duration::from_secs(/*secs*/ 1)
            )
            .await,
            Err("clipboard write timed out".into())
        );
        let pid = std::fs::read_to_string(pid_file).unwrap();
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "timed-out writer must be gone before another write"
        );
        let command = tokio::process::Command::new("cat");
        assert_eq!(
            super::write_clipboard_command(
                command,
                "fresh",
                Instant::now() + Duration::from_secs(/*secs*/ 5)
            )
            .await,
            Ok(())
        );
    }
}
