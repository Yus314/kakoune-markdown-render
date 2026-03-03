use crate::kak::{KakRange, format_commands};

/// daemon ロジックを kak -p 依存から切り離してテスト可能にする出力先抽象化。
pub trait EmitSink: Send {
    fn emit(
        &mut self,
        session:     &str,
        client:      &str,
        bufname:     &str,
        timestamp:   u64,
        width:       usize,
        conceal:     &[KakRange],
        faces:       &[KakRange],
        config_hash: u64,
    );
}

/// 本番用: emit_to_kak を呼び kak -p で送信する
pub struct KakPipeSink;

impl EmitSink for KakPipeSink {
    fn emit(
        &mut self,
        session:     &str,
        client:      &str,
        bufname:     &str,
        timestamp:   u64,
        width:       usize,
        conceal:     &[KakRange],
        faces:       &[KakRange],
        config_hash: u64,
    ) {
        emit_to_kak(session, client, bufname, timestamp, width, conceal, faces, config_hash);
    }
}

/// kak -p でコマンドを送信する。fire-and-forget（render スレッドをブロックしない）。
pub fn emit_to_kak(
    session:     &str,
    client:      &str,
    bufname:     &str,
    timestamp:   u64,
    width:       usize,
    conceal:     &[KakRange],
    faces:       &[KakRange],
    config_hash: u64,
) {
    let cmd     = format_commands(client, bufname, timestamp, width, conceal, faces, config_hash);
    let session = session.to_string();
    std::thread::spawn(move || {
        spawn_kak_p_with_timeout(&session, &cmd);
    });
}

/// kak -p でコマンドを送信する（2秒タイムアウト付き）。
fn spawn_kak_p_with_timeout(session: &str, cmd: &str) {
    fn spawn_kak_p(session: &str, cmd: &str) -> std::io::Result<std::process::ExitStatus> {
        use std::io::Write;
        let mut child = std::process::Command::new("kak")
            .args(["-p", session])
            .stdin(std::process::Stdio::piped())
            .spawn()?;
        {
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(cmd.as_bytes()).ok();
        }  // drop stdin → EOF → kak 終了
        child.wait()
    }

    // まず `timeout 2 kak -p` を試みる
    let result = std::process::Command::new("timeout")
        .args(["2", "kak", "-p", session])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            {
                let mut stdin = child.stdin.take().unwrap();
                stdin.write_all(cmd.as_bytes()).ok();
            }
            child.wait()
        });

    // `timeout` コマンドが存在しない場合のフォールバック
    if let Err(ref e) = result {
        if e.kind() == std::io::ErrorKind::NotFound {
            let session2 = session.to_string();
            let cmd2     = cmd.to_string();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(spawn_kak_p(&session2, &cmd2));
            });
            match rx.recv_timeout(std::time::Duration::from_secs(2)) {
                Ok(Ok(s)) if s.success() => return,
                _ => eprintln!("kak -p timed out or failed for session {}", session),
            }
            return;
        }
    }

    if !matches!(result, Ok(s) if s.success()) {
        eprintln!("kak -p failed or timed out for session {}", session);
    }
}
