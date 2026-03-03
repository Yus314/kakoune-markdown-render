use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;
use anyhow::Context;

use crate::config::Config;
use crate::paths::socket_path;

pub struct SendArgs {
    pub session:     String,
    pub bufname:     Option<String>,
    pub timestamp:   Option<u64>,
    pub cursor:      Option<usize>,
    pub width:       Option<usize>,
    pub client:      Option<String>,
    pub cmd_fifo:    Option<String>,
    pub ping:        bool,
    pub check_alive: bool,
    pub close:       bool,
    pub shutdown:    bool,
    pub config_hash: Option<String>,
}

pub fn run_send(args: &SendArgs) -> anyhow::Result<()> {
    let sock = socket_path(&args.session);

    // --check-alive: ソケット接続できれば exit 0、できなければ exit 1
    if args.check_alive {
        return UnixStream::connect(&sock)
            .map(|_| ())
            .with_context(|| format!("daemon not running: {}", sock.display()));
    }

    let mut stream = UnixStream::connect(&sock)
        .with_context(|| format!("daemon not running? {}", sock.display()))?;

    // タイムアウト設定（daemon 無応答時の kak フリーズを防止）
    stream.set_write_timeout(Some(Duration::from_millis(400)))?;
    stream.set_read_timeout(Some(Duration::from_millis(400)))?;

    // --shutdown: セッション全体を停止
    if args.shutdown {
        writeln!(stream, "SHUTDOWN\t{}", args.session)?;
        return Ok(());
    }

    // --close: 特定バッファの BufState を解放
    if args.close {
        let bufname = args.bufname.as_deref()
            .context("--close requires --bufname")?;
        writeln!(stream, "CLOSE\t{}\t{}", args.session, bufname)?;
        return Ok(());
    }

    if args.ping {
        let config_hash_str = args.config_hash.as_deref().unwrap_or("");
        writeln!(
            stream,
            "PING\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            args.session,
            args.bufname.as_deref().unwrap_or(""),
            args.timestamp.unwrap_or(0),
            args.cursor.unwrap_or(0),
            args.width.unwrap_or(0),
            config_hash_str,
            args.client.as_deref().unwrap_or(""),
            args.cmd_fifo.as_deref().unwrap_or(""),
        )?;
    } else {
        // RENDER: Config を環境変数から生成してヘッダに config_len を含める
        let config = Config::from_env().to_bytes();
        writeln!(
            stream,
            "RENDER\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            args.session,
            args.bufname.as_deref().unwrap_or(""),
            args.timestamp.unwrap_or(0),
            args.cursor.unwrap_or(0),
            args.width.unwrap_or(0),
            args.client.as_deref().unwrap_or(""),
            args.cmd_fifo.as_deref().unwrap_or(""),
            config.len(),
        )?;
        stream.write_all(&config)?;

        // Content は stdin から直接 UDS へストリーム（read_to_end 不使用）
        // 書き込みエラー（BrokenPipe/TimedOut/WouldBlock 等）が起きても
        // stdin を必ず drain し kak_response_fifo のライターブロックを解放する。
        if let Err(_) = io::copy(&mut io::stdin(), &mut stream) {
            io::copy(&mut io::stdin(), &mut io::sink()).ok();
        }
    }
    Ok(())
}
