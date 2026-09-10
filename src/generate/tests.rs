use std::{os::unix::fs::PermissionsExt, time::Duration};

use tokio_util::sync::CancellationToken;

use super::invoke_opencode;

fn fixture(script: &str) -> tempfile::TempDir {
    let workspace = tempfile::tempdir().unwrap();
    let shell = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|path| path.join("sh"))
        .find(|path| path.is_file())
        .expect("test shell on PATH");
    let binary = workspace.path().join("opencode");
    std::fs::write(&binary, format!("#!{}\n{script}\n", shell.display())).unwrap();
    std::fs::set_permissions(binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    workspace
}

#[tokio::test]
async fn captures_both_streams_beyond_pipe_capacity() {
    let workspace = fixture(
        "i=0; while [ $i -lt 8192 ]; do printf 'stdout payload 0123456789\\n'; printf 'stderr payload 0123456789\\n' >&2; i=$((i+1)); done",
    );
    let binary = workspace.path().join("opencode");
    let (log, code) =
        invoke_opencode(binary.to_str().unwrap(), workspace.path(), "fixture", "", "10s", CancellationToken::new()).await.unwrap();
    assert_eq!(code, Some(0));
    assert_eq!(log.matches("stdout payload").count(), 8192);
    assert_eq!(log.matches("stderr payload").count(), 8192);
}

#[tokio::test]
async fn timeout_stops_capture_with_inherited_pipes() {
    let workspace = fixture("sleep 5 & printf 'stdout ready\\n'; printf 'stderr ready\\n' >&2; wait");
    let binary = workspace.path().join("opencode");
    let error = tokio::time::timeout(
        Duration::from_secs(3),
        invoke_opencode(binary.to_str().unwrap(), workspace.path(), "fixture", "", "1s", CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap_err()
    .to_string();
    assert!(error.contains("timed out"), "{error}");
    assert!(error.contains("stdout ready"), "{error}");
    assert!(error.contains("stderr ready"), "{error}");
}
