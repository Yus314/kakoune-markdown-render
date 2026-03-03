# kakoune-markdown-render 実装計画

Kakoune でMarkdownを編集する際にUIをリッチにするプラグイン。
`replace-ranges` + `ranges` ハイライターによるインエディタWYSIWYG描画。

---

## 設計概要

### バイナリ・命名規則

| 種別 | プレフィックス | 例 |
|------|--------------|-----|
| バイナリ | `mkdr` | `mkdr daemon`, `mkdr send` |
| kakオプション | `mkdr_` | `mkdr_conceal`, `mkdr_heading_char` |
| kakフェイス | `Mkdr` | `MkdrHeading1`, `MkdrBold` |
| kakコマンド | `mkdr-` | `mkdr-enable`, `mkdr-disable` |

### アーキテクチャ

```
Kakoune (WinSetOption filetype=markdown → NormalIdle/InsertIdle/WinResize フック、per-window)
  │
  ├─ No-op 判定（%sh{} 内で環境変数のみ）
  │    ├─ timestamp 変化なし AND 幅変化なし AND
  │    │    (cursor_context=0 OR カーソル位置変化なし) → 完全 no-op
  │    ├─ timestamp 変化なし AND (幅変化 OR カーソル変化) → mkdr send --ping
  │    └─ timestamp 変化あり → mkdr send (--ping なし) < kak_response_fifo
  │
  ▼
mkdr send（短命プロセス）
  │  UDS SOCK_STREAM 接続（接続ごとに分離）
  ├─ PING: ヘッダのみ（~100byte）
  └─ RENDER: ヘッダ + config（kak_opt_* 環境変数）+ content（io::copy でストリーム）
     ※ read_to_end 不使用。EPIPE 時も stdin を最後まで drain して kak_response_fifo を解放。
  │
  ▼
mkdr daemon（UDS で常駐）
  ├─ Accept スレッド: 接続を受け付け、メッセージをパース、チャネルへ送信
  └─ Render スレッド: チャネルをドレイン（同一バッファの最新のみ保持）→ パース → emit
       ├─ キャッシュは未フィルタ版 KakRange（emit 直前にカーソルフィルタ適用）
       ├─ PING: last_rendered == ping.timestamp の場合のみキャッシュ再利用
       └─ emit: kak -p（暫定）または kak_command_fifo（将来的な最適化）
  │
  ▼
kak -p $session
  └─ evaluate-commands -client <client> -buffer <bufname> %[
         set-option window mkdr_conceal <timestamp> ...
         set-option window mkdr_faces   <timestamp> ...
     ]
  ▼
Kakoune (replace-ranges / ranges ハイライター、window スコープ)
```

### 二オプション方式

| オプション | スコープ | ハイライター | 用途 |
|-----------|---------|------------|------|
| `mkdr_conceal` | **window** | `replace-ranges` | 構文記号を置換・非表示 |
| `mkdr_faces` | **window** | `ranges` | 意味的フェイス適用 |

### タイムスタンプガード

`set-option window mkdr_conceal <timestamp>` は Kakoune が自動検証し、不一致なら黙って無視する。
出力は `evaluate-commands -client <client> -buffer <bufname>` でラップして文脈を確定させる。

---

## パフォーマンス設計

### レイテンシ内訳（実測値推定）

#### RENDER パス（バッファ変更時）

| ステップ | コスト |
|---------|--------|
| kak hook shell fork | ~1ms |
| mkdr send 起動（Rust リリースビルド） | ~2-5ms |
| UDS connect + ストリーム送信 | ~0.1-0.5ms |
| daemon: pulldown-cmark パース | ~0.01-0.1ms |
| daemon: kak -p 呼び出し | **5-20ms（最大ボトルネック）** |
| **合計** | **~8-27ms** |

#### PING / no-op パス（カーソル移動のみ）

| ケース | コスト |
|--------|--------|
| timestamp・幅・カーソル 全て不変（cursor_context=0） | **~0ms（完全 no-op）** |
| カーソル or 幅のみ変化（cursor_context>0 or 幅変化） | ~7-17ms（mkdr send + daemon + kak -p） |

### 完全 no-op の条件（3要素の全一致）

```
timestamp 変化なし
AND window_width 変化なし       ← 追加（幅変化時は thematic/code fence が崩れる）
AND (
    cursor_context=0            ← range がカーソル位置に依存しない
    OR cursor_line 変化なし     ← cursor_context>0 でもカーソルが動いていない
)
```

3つ全て満たす場合: `mkdr_last_timestamp`・`mkdr_last_width`・`mkdr_last_cursor_line` を
window オプションで管理し、`%sh{}` 内の環境変数比較だけで判定する。

### 最適化一覧

| 優先度 | 最適化 | 効果 | 実装箇所 |
|--------|--------|------|---------|
| ★★★ | 3要素 no-op（timestamp・幅・カーソル） | 大多数の Idle を ~0ms で処理 | M4/M6 kak hook |
| ★★★ | RENDER コアレスシング（2スレッド） | 連続タイプ時の無駄 kak -p を削減 | M5 daemon |
| ★★★ | キャッシュ未フィルタ化（emit 前フィルタ） | PING 再利用の正確性確保 | M5 daemon |
| ★★★ | mkdr_daemon_alive キャッシュ（PING の --check-alive スキップ） | PING 毎の 2-5ms を削減 | M4 options.kak / M5 response.rs |
| ★★★ | emit_to_kak fire-and-forget（spawn + detach） | render スレッドの 5-20ms ブロックを解消 | M5 response.rs |
| ★★ | UDS + io::copy（tmpfile・バイト計算不要） | 設計簡素化 + ~1-5ms 節約 | M4 send.rs |
| ★★ | PING 世代チェック（last_rendered 一致確認） | 誤ったキャッシュ適用を防止 | M5 daemon |
| ★★ | filter_cursor_overlap Cow（context=0 ゼロコピー） | デフォルトケースの Vec 確保を排除 | M5 render/mod.rs |
| ★★ | RENDER の sed 排除（kak_response_fifo にシングルクォート不含） | ~1ms の sed プロセス起動を排除 | M4 markdown-render.kak |
| ★★ | mkdr_last_config_hash キャッシュ（PING の config 変数 export を削減） | PING 時の 21 kak_opt_* export を 3 に削減 | M4 options.kak / M5 send.rs |
| ★★ | merge_message のキーを (String,usize) タプル化 | format!() による String 確保を排除 | M5 daemon/mod.rs |
| ★ | kak_command_fifo で kak -p 排除（将来） | 5-20ms のボトルネック排除 | M9 検討 |
| ★ | UDS/kak-p タイムアウト（400ms/2s） | UIフリーズ防止（信頼性） | M1 send.rs / M5 response.rs |
| ★ | idle_timeout チューニング | ユーザー調整余地 | M9 docs |

### RENDER コアレスシング（2スレッド設計）

単一スレッドでは `listener.incoming()` を逐次処理するため「処理中に latest_requested が更新されない」。
コアレスシングを実効させるには **accept スレッドと render スレッドを分離**する。

```rust
// main thread = accept thread
let (tx, rx) = std::sync::mpsc::channel::<Message>();
// state と sink の所有権を render スレッドに move する
std::thread::spawn(move || render_loop(rx, state, KakPipeSink));

for stream in listener.incoming() {
    match parse_message(BufReader::new(stream?)) {
        Ok(msg) => { tx.send(msg).ok(); }
        Err(ref e) if matches!(e.downcast_ref::<io::Error>(), Some(e) if e.kind() == io::ErrorKind::UnexpectedEof) => {}  // check_alive 接続 → silent ignore
        Err(e) => eprintln!("parse error: {e}"),
    }
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
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Message::Close(c)    => { state.remove_buf(&c.bufname); continue; }
                Message::Shutdown(_) => return,
                _ => merge_message(&mut pending, msg),
            }
        }
        for (_, msg) in pending.drain() {
            match msg {
                Message::Render(r) => handle_render(r, &mut state, &mut sink),
                Message::Ping(p)   => handle_ping(p, &mut state, &mut sink),
                _                  => unreachable!(),
            }
        }
    }
}
```

**注**: このコードは M5 の実装セクションと同一内容。パフォーマンス設計は概念説明に留め、
実装の詳細は M5 セクションを参照すること。

PING と RENDER が同一バッファに積まれた場合、RENDER を優先して PING を破棄する
（RENDER が最新情報を含んでいるため）。

### kak_command_fifo による kak -p 排除（将来の最適化候補）

`kak_command_fifo` のパスをメッセージに含め、daemon が直接書き込めば
`kak -p`（5-20ms）を排除できる可能性がある。ただし:

- `kak_command_fifo` は `%sh{}` の存続期間にのみ有効（背景プロセスがシェルを維持している間）
- 書き込むコマンドが PIPE_BUF (4096 bytes) を超えると複数ライター問題が再発
- 実現可能性は M6 実装後の計測結果を見て判断する（M9 の検討事項）

### セキュリティとソケットパス設計

```
# XDG_RUNTIME_DIR（systemd 管理、0700、tmpfs）を優先使用
$XDG_RUNTIME_DIR/mkdr/<session_hash>/daemon.sock

# フォールバック（/tmp を使う場合は uid 別ディレクトリ + 0700）
/tmp/mkdr-<uid>/<session_hash>/daemon.sock
```

`session_hash`: セッション名の FNV-1a ハッシュ先頭16文字（hex）。
`sockaddr_un.sun_path` の長さ制限（~108バイト）を確実に回避するため、
生のセッション名ではなくハッシュを使う。

---

## ディレクトリ構造

```
kakoune-markdown-render/
├── Cargo.toml
├── PLAN.md
├── README.md
├── src/
│   ├── main.rs          # CLIエントリポイント（daemon, send サブコマンド）
│   ├── config.rs        # 全設定構造体・デフォルト値・KEY=VALUE パース・from_env
│   ├── offset.rs        # バイトオフセット↔行列変換
│   ├── kak.rs           # KakRange, kakquote, escape_markup, format_commands
│   ├── send.rs          # mkdr send: UDS接続、io::copy ストリーム、EPIPE drain
│   ├── paths.rs         # セッションディレクトリ・ソケットパス計算（ハッシュ化）
│   ├── render/
│   │   ├── mod.rs       # Renderer、walk()、filter_cursor_overlap
│   │   ├── heading.rs   # ATX見出し（H1-H6）
│   │   ├── thematic.rs  # テーマ区切り（---/***）
│   │   ├── blockquote.rs # 引用ブロック（> ）
│   │   ├── list.rs      # 箇条書き・番号付きリスト
│   │   ├── task.rs      # タスクリストマーカー（[ ]/[x]）
│   │   ├── code_block.rs    # フェンスコードブロック
│   │   ├── strikethrough.rs # 打ち消し線（Milestone 2）
│   │   ├── emphasis.rs      # 強調・太字（Milestone 7）
│   │   ├── code_span.rs # インラインコード（Milestone 7）
│   │   ├── link.rs      # リンク（Milestone 7）
│   │   └── table.rs     # テーブル（Milestone 8）
│   └── daemon/
│       ├── mod.rs       # UDS listen、2スレッド（accept + render）
│       ├── state.rs     # セッション状態管理（未フィルタキャッシュ）
│       ├── protocol.rs  # PING/RENDER/CLOSE/SHUTDOWNパーサ
│       └── response.rs  # kak -p 経由レスポンス生成（2s タイムアウト付き）
└── rc/
    ├── markdown-render.kak  # メイン（hook・ライフサイクル）
    ├── options.kak          # declare-option 全定義
    ├── faces.kak            # set-face 全定義
    └── commands.kak         # mkdr-enable / mkdr-disable / mkdr-reload
```

---

## Cargo.toml

```toml
[package]
name = "mkdr"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "mkdr"
path = "src/main.rs"

[dependencies]
pulldown-cmark = { version = "0.13", features = [] }
clap = { version = "4", features = ["derive"] }
anyhow = "1"
libc = "0.2"   # getuid() に使用（/tmp フォールバック時の uid 別ディレクトリ生成）

[profile.release]
strip = true
opt-level = 3
lto = true
```

---

## Milestone 0 — プロジェクト初期化

**目標**: リポジトリとビルドが通る状態

### タスク

- [ ] `cargo init --name mkdr`
- [ ] `Cargo.toml` を上記内容で更新
- [ ] `src/` および `rc/` ディレクトリ作成
- [ ] `src/main.rs` に最小限のスタブ
- [ ] `cargo build` 成功確認
- [ ] `.gitignore`（`/target`）


---

## ファイル構成

このドキュメントは3ファイルに分割されています：

- **PLAN.md**（このファイル）— 設計概要・アーキテクチャ・パフォーマンス設計・ディレクトリ構造・M0
- **[PLAN-impl.md](PLAN-impl.md)** — Milestone 1〜5（コアインフラ・レンダリング・kak プラグイン・daemon）
- **[PLAN-reference.md](PLAN-reference.md)** — 技術的注意事項・フェイス/オプション一覧・M6〜M9
