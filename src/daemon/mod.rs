use std::collections::HashMap;
use std::io::{self, BufReader};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::Receiver;

use crate::config::Config;
use crate::paths::{fnv1a, ensure_session_dir, socket_path};
use crate::render::{Renderer, filter_cursor_overlap};

use self::protocol::{parse_message, Message, PingMsg, RenderMsg};
use self::response::{EmitSink, KakPipeSink};
use self::state::SessionState;

pub mod protocol;
pub mod response;
pub mod state;

pub fn run(session: &str) -> anyhow::Result<()> {
    run_with_sink(session, KakPipeSink)
}

/// テスト用エントリポイント: sink を差し替えることで kak なし統合テストが可能
pub fn run_with_sink(session: &str, sink: impl EmitSink + 'static) -> anyhow::Result<()> {
    ensure_session_dir(session)?;
    let sock_path = socket_path(session);

    // bind() 先行で試みる。EADDRINUSE なら stale チェックして再 bind。
    let listener = match UnixListener::bind(&sock_path) {
        Ok(l) => l,
        Err(e) if e.raw_os_error() == Some(libc::EADDRINUSE) => {
            if UnixStream::connect(&sock_path).is_ok() {
                return Ok(());  // 既に動いている daemon がいる
            }
            // 接続拒否 = stale socket → remove して再 bind
            std::fs::remove_file(&sock_path)?;
            UnixListener::bind(&sock_path)?
        }
        Err(e) => return Err(e.into()),
    };

    let (tx, rx) = std::sync::mpsc::channel::<Message>();
    let state = SessionState::default();

    // render スレッドに state と sink を move
    std::thread::spawn(move || render_loop(rx, state, sink));

    // Accept スレッド（メインスレッド）
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s)  => s,
            Err(e) => { eprintln!("accept error: {e}"); continue; }
        };
        match parse_message(BufReader::new(stream)) {
            Ok(msg) => { if tx.send(msg).is_err() { break; } }
            // UnexpectedEof = --check-alive 接続（ヘッダを書かずに即クローズ）: 正常
            Err(ref e) if matches!(
                e.downcast_ref::<io::Error>(),
                Some(e) if e.kind() == io::ErrorKind::UnexpectedEof
            ) => {}
            Err(e) => eprintln!("parse error: {e}"),
        }
    }

    let _ = std::fs::remove_file(&sock_path);
    Ok(())
}

fn render_loop(rx: Receiver<Message>, mut state: SessionState, mut sink: impl EmitSink) {
    loop {
        let first = match rx.recv() { Ok(m) => m, Err(_) => break };

        // Close/Shutdown はコアレスシングをスキップして即処理
        match first {
            Message::Close(c)    => { state.remove_buf(&c.bufname); continue; }
            Message::Shutdown(_) => return,
            _ => {}
        }

        let mut pending: HashMap<(String, usize), Message> = HashMap::new();
        merge_message(&mut pending, first);

        // 残りをノンブロッキングでドレイン（同一バッファは最新で上書き）
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Message::Close(c)    => { state.remove_buf(&c.bufname); continue; }
                Message::Shutdown(_) => return,
                _ => merge_message(&mut pending, msg),
            }
        }

        // バイナリ置換検知: 今回のリクエストは処理してから終了
        let should_exit = binary_replaced();

        // バッファごとに最新1件だけ処理
        for (_, msg) in pending.drain() {
            match msg {
                Message::Render(r) => handle_render(r, &mut state, &mut sink),
                Message::Ping(p)   => handle_ping(p, &mut state, &mut sink),
                _                  => unreachable!("Close/Shutdown は上で処理済み"),
            }
        }

        if should_exit {
            eprintln!("mkdr: binary replaced, exiting for restart");
            return;
        }
    }
}

/// バイナリが置き換えられたかチェック（Linux: /proc/self/exe が "(deleted)" で終わる）
#[cfg(target_os = "linux")]
fn binary_replaced() -> bool {
    std::fs::read_link("/proc/self/exe")
        .map(|p| p.to_string_lossy().ends_with("(deleted)"))
        .unwrap_or(false)
}

#[cfg(not(target_os = "linux"))]
fn binary_replaced() -> bool {
    false
}

/// PING と RENDER が競合する場合は RENDER を優先（キーは (bufname, width) タプル）。
fn merge_message(map: &mut HashMap<(String, usize), Message>, msg: Message) {
    debug_assert!(matches!(msg, Message::Ping(_) | Message::Render(_)));
    let key = (msg.bufname().to_string(), msg.width());
    match map.get(&key) {
        Some(Message::Render(_)) if matches!(msg, Message::Ping(_)) => {}  // RENDER 優先
        _ => { map.insert(key, msg); }
    }
}

fn handle_render(msg: RenderMsg, state: &mut SessionState, sink: &mut dyn EmitSink) {
    // デバッグダンプ: MKDR_DEBUG_DIR が設定されている場合、受信内容をファイルに書き出す
    if let Ok(dir) = std::env::var("MKDR_DEBUG_DIR") {
        let _ = std::fs::create_dir_all(&dir);
        let path = std::path::Path::new(&dir).join("content.bin");
        let _ = std::fs::write(&path, &msg.content);
        let config_path = std::path::Path::new(&dir).join("config.bin");
        let _ = std::fs::write(&config_path, &msg.config);
        let meta_path = std::path::Path::new(&dir).join("meta.txt");
        let _ = std::fs::write(&meta_path, format!(
            "session={}\nbufname={}\ntimestamp={}\ncursor={}\nwidth={}\nclient={}\ncontent_len={}\nconfig_len={}\n",
            msg.session, msg.bufname, msg.timestamp, msg.cursor, msg.width, msg.client,
            msg.content.len(), msg.config.len(),
        ));
    }

    let config      = Config::from_bytes(&msg.config).unwrap_or_default();
    let config_hash = fnv1a(&msg.config);

    let (conceal, faces) =
        Renderer::new(&msg.content, &config, msg.width).render_unfiltered();

    {
        let fc = filter_cursor_overlap(&conceal, msg.cursor, config.cursor_context);
        let ff = filter_cursor_overlap(&faces,   msg.cursor, config.cursor_context);
        sink.emit(
            &msg.session, &msg.client, &msg.bufname,
            msg.timestamp, msg.width, fc.as_ref(), ff.as_ref(), config_hash,
        );
    }

    // キャッシュは未フィルタ版を保存
    let buf = state.get_buf_mut(&msg.bufname, msg.width);
    buf.last_rendered       = msg.timestamp;
    buf.last_config_hash    = config_hash;
    buf.last_cursor_context = config.cursor_context;
    buf.cached_conceal      = conceal;
    buf.cached_faces        = faces;
}

fn handle_ping(msg: PingMsg, state: &mut SessionState, sink: &mut dyn EmitSink) {
    let buf = match state.get_buf(&msg.bufname, msg.width) {
        Some(b) => b,
        None    => return,
    };

    // 世代 + 設定変更チェック
    let Some((cached_ts, cached_hash)) = parse_config_hash_str(&msg.config_hash_str) else {
        return;
    };
    if buf.last_rendered    != cached_ts   { return; }
    if buf.last_config_hash != cached_hash { return; }

    let cursor_context = buf.last_cursor_context;
    let config_hash    = buf.last_config_hash;

    let fc = filter_cursor_overlap(&buf.cached_conceal, msg.cursor, cursor_context);
    let ff = filter_cursor_overlap(&buf.cached_faces,   msg.cursor, cursor_context);

    sink.emit(
        &msg.session, &msg.client, &msg.bufname,
        msg.timestamp, msg.width, fc.as_ref(), ff.as_ref(), config_hash,
    );
}

/// `"{ts:016x}:{hash:016x}"` 形式から (ts, hash) を取り出す。
fn parse_config_hash_str(s: &str) -> Option<(u64, u64)> {
    let (ts_hex, hash_hex) = s.split_once(':')?;
    let ts   = u64::from_str_radix(ts_hex,   16).ok()?;
    let hash = u64::from_str_radix(hash_hex, 16).ok()?;
    Some((ts, hash))
}
