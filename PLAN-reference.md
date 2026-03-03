# kakoune-markdown-render — リファレンス

> 設計概要は [PLAN.md](PLAN.md)、実装詳細は [PLAN-impl.md](PLAN-impl.md) 参照

## 技術的注意事項

### pulldown-cmark のバイト範囲

- `Range<usize>` は **end 排他的**（Rust 標準）
- Kakoune の range-specs は **end 包含的**
- → `byte_to_line_col(starts, range.end - 1)` で end 変換（off-by-one 必須対応）
- `Start` と `End` イベントは同一 range（要素全体）を持つ
- マーカーバイト長は固定仮定せずソースから実測する

### UDS ソケットパスの長さ制限

`sockaddr_un.sun_path` は OS によって異なるが Linux では 108 バイト。
`$XDG_RUNTIME_DIR/mkdr/{session}/daemon.sock` はセッション名が長いと超過する。
→ セッション名を FNV-1a ハッシュ（16文字 hex）に変換して使用する。
`paths.rs` が全パス計算を担い、kak 側も `mkdr send` に任せてパスを直接構築しない。

### /tmp フォールバック時のセキュリティ

`$XDG_RUNTIME_DIR` は systemd が管理する tmpfs（0700）であり安全。
フォールバックで `/tmp` を使う場合は `/tmp/mkdr-{uid}/` + `chmod 0700` が必須。
そうしないと別ユーザーがソケットに接続して Kakoune へコマンドを注入できる。

### 「ソケットは存在するが daemon が死んでいる」の検知

`-S sock` だけではソケットファイルが残留しているが daemon が死んでいるケースを拾えない。
`mkdr send --check-alive` で実際に接続を試み、失敗したら残留ソケットを削除して再起動する。

### EPIPE 時の stdin drain（kak_response_fifo のデッドロック防止）

daemon が早期クローズした場合、`mkdr send` の UDS 書き込みが EPIPE で失敗する。
このとき stdin（= kak_response_fifo）が読まれなくなり、Kakoune の `write $response_fifo`
が詰まってセッションが固まる。→ EPIPE を捕捉して `io::copy(stdin, io::sink())` で drain する。

### ストリーミング送信（content_len 不要）

RENDER の content は接続 EOF まで読む設計にすることで:
- `mkdr send` が `read_to_end` せず `io::copy(stdin→socket)` できる（ゼロコピーに近い）
- content_len を shell 側で計算する必要がなくなる（`${#var}` のバイト数問題を回避）

### キャッシュは未フィルタ版を保持

daemon の `BufState` に保存するキャッシュは **カーソルフィルタを掛ける前** の KakRange リスト。
PING で再利用する際に新しいカーソル位置でフィルタを掛け直すため、未フィルタが必要。
フィルタ済みをキャッシュすると、カーソルが動いたときに以前の近傍 range が永久欠損する。

### PING 世代チェックと config_hash 検証

PING を受け取ったとき以下の2条件でキャッシュ再利用をスキップする：

`kak_opt_mkdr_last_config_hash` は `"{ts:016x}:{hash:016x}"` 形式（`config_hash_str`）で保存され、
`parse_config_hash_str()` で ts と hash の両方を同時に検証する。

1. `buf.last_rendered != cached_ts`（`config_hash_str` から抽出した `ts`）
   → RENDER 完了前に PING が来た場合、または fire-and-forget の古い kak -p が
     `config_hash_str` を上書きした場合。timestamp 不一致でスキップ。

2. `buf.last_config_hash != cached_hash`（`config_hash_str` から抽出した `hash`）
   → 設定変更後の RENDER が完了する前に PING が来た場合。
     kak 側の `GlobalSetOption` フックが `config_hash_str` をリセットして次回 RENDER を
     強制するが、RENDER 到着前の PING で古い設定のキャッシュが適用されることを防ぐ。

`ts` を含めることで、fire-and-forget で並走する複数の kak -p が
`mkdr_last_config_hash` を古い値で上書きしても、世代不一致により PING が
誤ったキャッシュを使い続ける問題（永続的 PING スキップ）を防げる。

さらに `cursor_context` は `Config::default()` ではなく `buf.last_cursor_context`
（RENDER 時に保存した値）を使う。`Config::default()` を使うと `cursor_context` 変更直後の
PING で誤フィルタが掛かりカーソル近傍の range が漏れる（または欠落する）バグになる。

### コアレスシング（2スレッド設計）

accept スレッドがメッセージを channel に送り、render スレッドが `try_recv()` でドレインして
同一バッファの最新1件のみ処理する。PING と RENDER が競合した場合は RENDER を優先。
これにより連続タイプ時の中間フレームで無駄な kak -p が呼ばれなくなる。

### 幅変化の強制 RENDER

`thematic break` や `code fence` の border 文字列は `window_width` 依存。
`WinResize` フックで `mkdr_last_width` と `mkdr_last_timestamp` の**両方**をリセットし
次の Idle で RENDER を強制する。`mkdr_last_width` だけリセットすると、
`ts_same=1` かつ `w_same=0` の条件で PING パスに入り、キャッシュが再利用されるため
幅依存の置換が更新されない。

同一バッファを異なる幅の window で開いている場合、daemon のコアレスシングは
`(bufname, width)` をキーにしているため、それぞれの幅で独立してレンダリングされる。

### カーソル行フィルタ

`filter_cursor_overlap()` は `line_start` と `line_end` の両端で重なり判定:

```rust
.filter(|r| r.line_end < lo || r.line_start > hi)
```

`line_start` のみの判定ではコードブロック等の複数行 range が漏れる。

### タイムアウト設計（UIフリーズ防止）

daemon が応答しない場合に `mkdr send` の UDS 書き込みが永久にブロックすると
Kakoune の `%sh{}` が詰まりエディタが固まる。これを防ぐために2段のタイムアウトを設ける。

| 箇所 | タイムアウト | 実装 |
|------|------------|------|
| `mkdr send` UDS 書き込み | **400ms** | `stream.set_write_timeout(Duration::from_millis(400))` |
| `mkdr send` UDS 読み込み | **400ms** | `stream.set_read_timeout(Duration::from_millis(400))` |
| daemon `kak -p` 呼び出し | **2s** | `timeout 2 kak -p SESSION`（Linux `timeout` コマンド経由） |

タイムアウト発生時は EPIPE 扱いとして stdin drain して正常終了する（Kakoune に
エラーは伝えない。次の Idle で自動リトライされる）。

### タイムスタンプガード

`set-option window mkdr_conceal <timestamp>` は Kakoune が自動検証する。
さらに `evaluate-commands -client <client> -buffer <bufname>` でラップして文脈を確定する。

### `GlobalSetOption` フックの制限（全 window への伝播なし）

`hook global GlobalSetOption mkdr_.*` 内の `set-option window` は
**現在アクティブな window にのみ適用**される。他の markdown window のキャッシュは
リセットされないため、設定変更後に以下のような UX 上の制限がある：

- 非アクティブな markdown window は、次に buffer が変更されるまで古い設定で表示される
- daemon の `config_hash` チェックにより**正確性は保証**される（古いキャッシュは PING で適用されない）
- 実際の表示更新は「次回その window で編集を行った時」か「`mkdr-reload` コマンド実装後」

M9 で `mkdr-reload` コマンド（全 window のキャッシュリセット + 強制 RENDER）を実装して
この制限を緩和する。

### `ENABLE_SMART_PUNCTUATION` の注意

```rust
Options::ENABLE_SMART_PUNCTUATION  // "..." → "…"、-- → –、--- → — 等を変換
```

pulldown-cmark の `into_offset_iter()` はソース上のバイトオフセットを返すため、
Text イベントの内容が変換されてもオフセット計算には影響しない。
ただし **Text イベントの content（変換後のテキスト）をソース上のバイト範囲の参照に
使ってはならない**（例：変換後の文字列長でバイト計算するのは誤り）。
各レンダラーモジュールは常に `content[range]` でソースを参照すること。

### `%sh{}` 内の環境変数エクスポート

Kakoune は `%sh{}` 内で**明示的に参照された変数のみ**を shell 環境に export する。
`mkdr send` が `Config::from_env()` で `kak_opt_mkdr_*` を読むためには、
`%sh{}` の先頭でオプションを `: "${kak_opt_mkdr_*}"` 形式で明示参照する必要がある。
参照しなかった場合、`mkdr send` はデフォルト設定で動作し「設定が効かない」致命的症状になる。

**RENDER vs PING の参照分離（パフォーマンス最適化）**:
- **RENDER パス**: 21 個の config オプション全てを参照する（`Config::from_env()` で読む）
- **PING パス**: `mkdr_cursor_context`・`mkdr_daemon_alive`・`mkdr_last_config_hash` の3つのみ
  - `--config-hash "$kak_opt_mkdr_last_config_hash"` を `send.rs` に渡すことで
    `Config::from_env().hash()` の計算と 21 変数 export を省略できる
  - `send.rs` は文字列をそのまま PING メッセージに転送するだけ（パース不要）
  - 空の場合: daemon の `parse_config_hash_str()` が `None` を返し PING をスキップする（正常）

### `mkdr_daemon_alive` キャッシュと daemon 復旧

`mkdr_daemon_alive` は PING パスで `--check-alive` (2-5ms) をスキップするためのキャッシュ。
- **設定タイミング**: RENDER パスが `--check-alive` 成功後に `set-option window mkdr_daemon_alive true` を出力
- **リセットタイミング**: 実装上は自動リセットしない（window スコープはセッション終了時に消える）
- **daemon 死亡後の挙動**: PING は `mkdr send --ping` が background で失敗（silent）。
  次回 RENDER パス（buffer 変更時）が `--check-alive` で死亡を検知し daemon を再起動・`mkdr_daemon_alive true` を再設定。
- **注意**: daemon 死亡から次の RENDER まで PING は全て空振りになる（カーソルフィルタ更新なし）。
  重大な問題ではなく、次回編集時に自然に回復する。

### `emit_to_kak` fire-and-forget と render スレッドのブロッキング

`emit_to_kak` は `std::thread::spawn` で別スレッドに `kak -p`（5-20ms）を委譲し、
render スレッドは即座に次のメッセージ処理に移る。

**なぜ重要か**: render スレッドが kak -p をブロッキングで待つと、
その間に channel に積まれたメッセージはコアレスシングされない。
例えば 100 件の RENDER が積まれていても、1件処理するたびに 10ms ブロックすると
合計 1 秒かかり、その間に新しい RENDER が届いても前の kak -p 完了まで処理できない。
fire-and-forget により、render スレッドは kak -p の完了を待たずにドレインし続けられる。

**副作用**: kak -p スレッドが複数同時に走る場合がある。
ただし `evaluate-commands -client <client> -buffer <bufname>` + タイムスタンプガードで
順序依存を排除しているため、複数の kak -p が競合しても最終的な表示は正確になる。

### markup string の `{` エスケープ

`replace-ranges` の置換文字列は markup string として解釈されるため、
`{Face}text{/Face}` の構文が適用される。リテラルとして `{` を表示するには `\{` が必要。
`escape_markup()` は `\` → `\\`、`|` → `\|`、`{` → `\{` の順で処理する。
（`\` を最初に処理しないと後続の置換が二重エスケープになる）

### WinResize → 強制 RENDER

`mkdr-on-resize` では `mkdr_last_width` に加えて `mkdr_last_timestamp` も
空文字列にリセットする。`mkdr_last_width` だけリセットすると、
`ts_same=1` かつ `w_same=0` → PING パスに入り、幅依存の thematic break /
code fence border が古い幅のままになる（PING はキャッシュ再利用のみ）。
`mkdr_last_timestamp` をリセットすることで `ts_same=0` → RENDER パスが強制される。

### `-try-client` と `try-catch` によるエラー無視

`kak -p` で送るコマンドは `try %{ eval -try-client ... %{ ... } } catch %{}` でラップする。
- `-try-client`：client が既に閉じていてもエラーにならない
- `try ... catch %{}`：buffer が閉じている等の想定外エラーを黙殺し daemon が詰まらない

### BufClose による daemon 状態のクリーンアップ

`hook global BufClose` で `mkdr send --close` を送り、daemon の `BufState` を解放する。
送らない場合、daemon にバッファ状態が残り続けメモリリークになる。また閉じた buffer/client
への `kak -p` が無駄に走り続けエラーログが汚れる。

### 設定の Unset 規約

Kakoune にはスコープ間の「継承削除」がないため、`buffer` スコープで設定した値を
`global` デフォルトに戻す手段として **空文字列 = "unset"** を規約とする。

```kak
# buffer スコープで設定を上書き
set-option buffer mkdr_thematic_char '='

# global デフォルトに戻す（unset）
set-option buffer mkdr_thematic_char ''
```

`Config::from_env()` は環境変数が空文字列のとき Built-in デフォルト値を使用する。
これにより `global → buffer` のスコープ継承で実質的なデフォルト回帰が実現できる。
bool 型オプションも同様に空文字列で Built-in デフォルトに戻る（`auto` 値は設けない）。

### daemon 二重起動の防止（bind() 原子性による）

複数の markdown バッファを同時に開いた場合、複数の `%sh{}` が並走して
`mkdr daemon` を二重起動しようとすることがある。

**対策**: `mkdr daemon` 内の `UnixListener::bind()` の原子性を利用する。
- 先に bind() した daemon のみがソケットを所有し起動に成功する
- 後から bind() しようとした daemon は `EADDRINUSE` で失敗し即終了する
- kak 側のポーリング（`--check-alive` ループ）は「誰かが起動した daemon」に接続できれば成功

したがって kak 側では以下のシンプルな実装で十分（mkdir ロック等の追加機構は不要）：
```sh
if ! mkdr send --check-alive --session "$kak_session" 2>/dev/null; then
    mkdr daemon --session "$kak_session" >/dev/null 2>&1 &  # 並走しても bind() で解決
    for _ in 1..10; do sleep 0.05; done  # 起動待ち
fi
```

注意: 残留ソケット（daemon が異常終了した場合）は `mkdr daemon` 側で `bind()` → EADDRINUSE →
`UnixStream::connect()` で stale 確認 → 接続失敗なら `remove_file` して再 `bind()` する。
先に `remove_file` してから `bind()` する実装は稼働中 daemon のソケットを破壊するリスクがある。

### `check_alive` 接続による daemon ログ汚染の防止

`mkdr send --check-alive` は接続後に何も送らず即切断する（EOF）。
daemon 側の `parse_message` が `UnexpectedEof` を返すため、素朴に実装すると
`eprintln!("parse error")` が NormalIdle のたびに出力される。

`parse_message` または accept ループで `io::ErrorKind::UnexpectedEof` を silent ignore:

```rust
// accept ループ（daemon/mod.rs / パフォーマンス設計セクション参照）
match parse_message(BufReader::new(stream?)) {
    Ok(msg) => { tx.send(msg).ok(); }
    Err(e) if e.root_cause().downcast_ref::<io::Error>()
                .map(|e| e.kind() == io::ErrorKind::UnexpectedEof)
                .unwrap_or(false) => {}  // check_alive 接続 → silent ignore
    Err(e) => eprintln!("parse error: {e}"),
}
```

### `mkdr-enable` の二重呼び出し対策

`WinSetOption filetype=markdown` フックは稀に2回発火することがある（filetype が
`markdown` に設定された後に再度設定される場合等）。
`add-highlighter window/mkdr-conceal` が2回呼ばれると「既に存在する」エラーになるため、
`try %{ add-highlighter ... }` でラップして安全に無視する。

### `BufClose` フックのバッファ選択

`hook global BufClose .*` はセッション内の**全バッファ**のクローズで発火する。
`kak_opt_filetype` を `%sh{}` 内でチェックして `markdown` 以外は即 exit し、
非 markdown バッファで無駄に `mkdr send` が起動されることを防ぐ。

### render_loop の所有権設計

`std::thread::spawn(move || render_loop(rx, state))` は `state` の所有権を
render スレッドに **move** する。accept スレッド（main）は channel の送信端
`tx` のみ保持し、`state` へは直接アクセスしない。
`&mut state` を `spawn` クロージャに渡そうとするとコンパイルエラーになる
（`'static` ライフタイムを満たせない）。

### kak -p の代替（将来検討）

`kak_command_fifo` のパスをメッセージに含めておくことで（プロトコルに既に追加済み）、
M9 で実現可能性を検証する。制約: FIFO への書き込みが PIPE_BUF (4096) 超になると
複数ライター問題が再発するため、大きなコマンドには別途対策が必要。

### テスト可能性の設計原則

**テスト可能設計のチェックリスト（実装時に確認）:**

| 設計決定 | 理由 |
|---------|------|
| `render_unfiltered()` → 純粋関数（`&self`, `kak -p` なし） | レンダリングロジックを直接テスト可能 |
| `parse_message(impl BufRead)` → I/O 抽象化 | `&[u8]` を渡してテスト可能 |
| `filter_cursor_overlap()` → 純粋関数 + `context=0` early-return | ドメインロジックをテスト可能、usize アンダーフロー防止 |
| `EmitSink` trait → `KakPipeSink`（本番）/ `RecordSink`（テスト） | `handle_render`/`handle_ping` を kak なしでテスト可能 |
| `Config::from_env_inner(lookup)` → `from_env` / `from_pairs` 共通 | 環境変数を汚染せずに Config をテスト可能 |
| `format_commands()` → `pub fn`（単独呼び出し可能） | 出力フォーマットを単体テスト可能 |
| `escape_markup()` → `pub fn`（純粋関数） | エスケープ順序のバグを単体テスト可能 |
| `daemon::run_with_sink(session, sink)` → テスト用エントリポイント | kak なし統合テストを可能に |
| `unique_session()` + `DaemonHandle` → テスト隔離 | 並列 `cargo test` での EADDRINUSE 防止 |

**純粋関数として維持すべき関数（副作用を混入させない）:**
- `render_unfiltered`、`filter_cursor_overlap`、`byte_to_line_col`
- `escape_markup`、`format_commands`、`fnv1a`、`Config::hash`

**外部副作用を持つ関数（テスト時は差し替え）:**
- `emit_to_kak` → `EmitSink::emit` でラップ
- `Config::from_env` → `from_env_inner` に委譲

---

## Milestone 6 — パフォーマンスベンチマーク（中間計測）

**目標**: 実際のレイテンシを計測し、目標値と照合する

### 目標値

| パス | 目標 |
|------|------|
| RENDER（バッファ変更時） | < 30ms |
| PING（cursor_context=0）完全 no-op | < 1ms |
| PING（cursor_context>0、カーソル変化あり） | < 20ms |
| 1万行バッファのパース | < 1ms |
| kak -p 呼び出し単体 | 計測・文書化 |

### 計測方法

#### pulldown-cmark パース単体（cargo bench）

```rust
// benches/parse_bench.rs
use criterion::{criterion_group, criterion_main, Criterion};

fn bench_parse(c: &mut Criterion) {
    let content_1k  = "# heading\n".repeat(100);
    let content_10k = "# heading\n".repeat(1000);
    c.bench_function("parse_1k_lines",  |b| b.iter(|| {
        Renderer::new(&content_1k, &Config::default(), 80).render_unfiltered()
    }));
    c.bench_function("parse_10k_lines", |b| b.iter(|| {
        Renderer::new(&content_10k, &Config::default(), 80).render_unfiltered()
    }));
}
criterion_group!(benches, bench_parse);
criterion_main!(benches);
```

```toml
# Cargo.toml に追加
[dev-dependencies]
criterion = "0.5"

[[bench]]
name = "parse_bench"
harness = false
```

#### kak -p 単体レイテンシ

```sh
# 空コマンドを送って kak -p の往復時間を計測
time echo 'nop' | kak -p <session>
```

#### RENDER エンドツーエンド（Kakoune 上で計測）

```kak
# Kakoune 内でタイムスタンプを記録し mkdr-render 前後の差を見る
hook -once window NormalIdle .* %{
    echo -debug "before render: %val{timestamp}"
    mkdr-render
}
# daemon の kak -p が届いた直後（set-option が実行されたタイミング）を
# WinSetOption mkdr_conceal フックで計測
hook -once window WinSetOption mkdr_conceal=.* %{
    echo -debug "after set: %val{timestamp}"
}
```

---

## Milestone 7 — インライン要素

**目標**: emphasis, code span, link のレンダリング

### `src/render/emphasis.rs`

pulldown-cmark の `Start(Tag::Emphasis)` / `Start(Tag::Strong)` の range は
**マーカーを含む要素全体**（`*text*` の `*text*` 全体）を指す。
`End(Tag::Emphasis)` の range も同じ全体 range を返す（Start と等しい）。

マーカーバイト長はソースから実測：

```rust
// emphasis の場合（* または _）
fn marker_len(content: &str, offset: usize) -> usize {
    let bytes = content.as_bytes();
    let marker = bytes[offset];  // '*' または '_'
    bytes[offset..].iter().take_while(|&&b| b == marker).count()
}
// *** bold italic の場合は 3、** bold は 2、* italic は 1
```

**Start イベントのみで処理する**（End は不要）：
- `range.start` から marker_len バイト → 開始マーカー → conceal で空文字列に置換
- `range.end - marker_len` バイトから末尾 → 終了マーカー → conceal で空文字列に置換
- 開始マーカー直後〜終了マーカー直前 → テキスト部分 → faces に追加

`***text***`（bold italic）は `Emphasis(Strong(...))` の入れ子ではなく
単一の `Strong(Emphasis(...))` イベントになる場合がある。
実装では `Start(Tag::Strong)` の range のマーカー長を実測して `MkdrBold` を適用し、
`Start(Tag::Emphasis)` の range から `MkdrItalic` を重ねる。
重複範囲の解決は `MkdrBoldItalic` フェイスを1エントリで追加する（faces 側）。

### `src/render/code_span.rs`

- バッククォートの連続長は可変（1〜N個）。**固定1バイト除去は誤り**。
- `&content[range.start..]` で `` ` `` が連続する長さを数え、末尾側も同長を除去

### `src/render/link.rs`

Milestone 7 では **インラインリンク `[text](url)` のみ**対応。

| リンク形式 | Milestone 7 | Milestone 8+ |
|-----------|-------------|--------------|
| `[text](url)` インライン | ✅ | - |
| `[text][ref]` 参照 | ❌ | 検討 |
| `<url>` オートリンク | ❌ | 検討 |

- `LinkType::Inline` のみ処理（`link_type` で判別してスキップ）
- URL 部分（`](url)`）を空に置換（conceal）、テキスト部に `MkdrLink` 適用

---

## Milestone 8 — 複合要素

**目標**: テーブル、setext見出し、参照リンク

### `src/render/table.rs`

- GFMテーブル（`ENABLE_TABLES` 必須）
- ヘッダ行・区切り行・データ行を色分け
- **実装前に確認が必要**: pulldown-cmark の `into_offset_iter()` が `TableCell` イベントに
  対してカラム単位の正確な range を返すか検証する。テーブルの `|` 区切りを含む行全体の
  range しか得られない場合は、ソースをスキャンして `|` を手動検索する必要がある。
  → M8 着手時に `println!("{:?}", event, range)` でデバッグ確認を行うこと。

### setext 見出し（`heading.rs` 拡張）

- pulldown-cmark は setext/ATX を区別しない
- range から下線行（`===`/`---`）を手動検索・空に置換（conceal）

### 参照リンク・オートリンク

- `LinkType::Reference` 等 → フェイス適用のみ（conceal なし）

---

## Milestone 9 — 品質・リリース準備

**目標**: 安定したリリース品質

### タスク

- [ ] エラーハンドリングの徹底（`anyhow::Context` で全 unwrap を排除）
- [ ] ログ機能（`daemon.log`、レベル制御）
- [ ] `mkdr send --check-alive` の実装（ソケット接続テスト）
- [ ] パニック時の自動デーモン再起動
- [ ] `mkdr-status` コマンド（デーモン生死・最終レンダリング時刻・バッファ数）
- [ ] ソケットクリーンアップ（残留ソケットファイル対策）
- [ ] `kak -p` への大きな range-specs 送信時の上限検証（巨大 Markdown での性能劣化対策）
- [ ] `mkdr-reload` コマンド実装（全 window のキャッシュリセット + 強制 RENDER）
  ```kak
  # 実装方針: client-list を iterate して全クライアントのキャッシュをリセット
  define-command mkdr-reload -docstring 'Force re-render in all markdown windows' %{
      evaluate-commands -buffer * %{
          evaluate-commands -client * %{
              # markdown window のみに適用
              evaluate-commands %sh{ [ "$kak_opt_filetype" = "markdown" ] || exit 0
                  printf 'set-option window mkdr_last_timestamp ""\n'
                  printf 'set-option window mkdr_last_width ""\n'
              }
          }
      }
      mkdr-render
  }
  ```
- [ ] BufSetOption フックによる buffer スコープ設定変更の検出（`GlobalSetOption` では buffer スコープの変更を拾えない）
- [ ] `kak_command_fifo` による kak -p 排除の可能性検証（Milestone 6 計測後に判断）
- [ ] プリセット実装（`mkdr_preset = default | minimal | ascii`）
  - `default`: Unicode 文字を使用（▌▎•◦▸☐☑─ 等）
  - `minimal`: シンプルな Unicode（見出しは `#` 保持、区切りは `-`）
  - `ascii`: ASCII 文字のみ（Unicode 非対応端末向け、`>` `*` `-` `[x]` 等）
- [ ] パフォーマンス回帰テスト（Milestone 6 の目標値を CI で確認）
- [ ] `README.md` 作成（インストール・設定・スクリーンショット）
  ```
  # インストール手順（README に記載する内容）
  1. cargo build --release
  2. cp target/release/mkdr ~/.local/bin/  # PATH の通った場所に置く
  3. mkdir -p ~/.config/kak/autoload
  4. ln -s /path/to/kakoune-markdown-render/rc ~/.config/kak/autoload/mkdr
     # または require-module で手動ロード
  5. Kakoune を起動して .md ファイルを開く → 自動で mkdr-enable が呼ばれる
  ```
- [ ] idle_timeout チューニングガイドの文書化
- [ ] CI 設定（`cargo test`, `cargo clippy`, `cargo fmt --check`）
- [ ] nixpkgs パッケージ化検討

---

## 依存関係グラフ

```
M0 (プロジェクト初期化)
  └─ M1 (コアインフラ + UDS基盤 + paths.rs)
       ├─ M2 (ブロック要素)
       │    └─ M3 (バイナリ完成: daemon + send)
       │         └─ M4 (kak plugin) ← 検証ポイント1
       │              └─ M5 (daemon 2スレッド + コアレスシング)
       │                   └─ M6 (パフォーマンス計測) ← 検証ポイント2
       │                        ├─ M7 (インライン要素)
       │                        └─ M8 (複合要素)
       │                             └─ M9 (品質・リリース)
       └─ (config.rs は M2 と並行開発可能)
```

---

## フェイス定義一覧（~31個）

| フェイス名 | デフォルト | 用途 |
|-----------|-----------|------|
| `MkdrHeading1` | `default+b` | H1見出し |
| `MkdrHeading2` | `default+b` | H2見出し |
| `MkdrHeading3` | `default+bi` | H3見出し |
| `MkdrHeading4` | `default+i` | H4見出し |
| `MkdrHeading5` | `default` | H5見出し |
| `MkdrHeading6` | `default` | H6見出し |
| `MkdrBold` | `default+b` | 太字 |
| `MkdrItalic` | `default+i` | イタリック |
| `MkdrBoldItalic` | `default+bi` | 太字イタリック（重なり解決用） |
| `MkdrStrikethrough` | `default+s` | 打ち消し線 |
| `MkdrCode` | `default` | インラインコード |
| `MkdrCodeBlock` | `default` | コードブロック本体 |
| `MkdrCodeLang` | `default+i` | 言語文字列 |
| `MkdrCodeBorder` | `default` | フェンス行 |
| `MkdrBlockQuote` | `default+i` | 引用ブロック |
| `MkdrBlockQuoteMarker` | `default` | 引用マーカー（▎） |
| `MkdrThematicBreak` | `default` | テーマ区切り |
| `MkdrBullet` | `default` | 箇条書きマーカー |
| `MkdrOrderedList` | `default` | 番号付きリスト番号 |
| `MkdrTaskChecked` | `green` | チェック済みタスク |
| `MkdrTaskUnchecked` | `default` | 未チェックタスク |
| `MkdrLink` | `blue+u` | リンクテキスト |
| `MkdrLinkUrl` | `default` | リンクURL |
| `MkdrLinkTitle` | `default+i` | リンクタイトル |
| `MkdrImage` | `magenta` | 画像 |
| `MkdrTableHeader` | `default+b` | テーブルヘッダ |
| `MkdrTableBorder` | `default` | テーブル罫線（\|） |
| `MkdrTableDelimiter` | `default` | テーブル区切り行 |
| `MkdrTableRow` | `default` | テーブルデータ行 |
| `MkdrHtmlBlock` | `default` | HTMLブロック |
| `MkdrHtmlInline` | `default` | インラインHTML |

---

## Kakouneオプション一覧

| オプション名 | スコープ | 型 | デフォルト | 説明 |
|------------|---------|-----|---------|------|
| `mkdr_conceal` | **window** | range-specs | - | replace-ranges ハイライター用 |
| `mkdr_faces` | **window** | range-specs | - | ranges ハイライター用 |
| `mkdr_last_timestamp` | **window** | str | `` | no-op 判定用タイムスタンプキャッシュ |
| `mkdr_last_width` | **window** | str | `` | no-op 判定用幅キャッシュ |
| `mkdr_last_cursor_line` | **window** | str | `` | no-op 判定用カーソル行キャッシュ |
| `mkdr_daemon_alive` | **window** | bool | `false` | daemon 生存キャッシュ（PING の --check-alive スキップ） |
| `mkdr_last_config_hash` | **window** | str | `` | `"{ts_hex}:{hash_hex}"` 複合値。世代チェック（fire-and-forget 競合防止）+ config 変更検出の両用 |
| `mkdr_cursor_context` | buffer | int | `0` | カーソル周辺の非レンダリング行数 |
| `mkdr_preset` | buffer | str | `default` | プリセット名 |
| `mkdr_heading_char_1..6` | buffer | str | `▌` | H1-H6プレフィックス文字 |
| `mkdr_heading_setext` | buffer | bool | `false` | setext形式見出しの有効化 |
| `mkdr_thematic_char` | buffer | str | `─` | テーマ区切り文字 |
| `mkdr_blockquote_char` | buffer | str | `▎` | 引用マーカー文字 |
| `mkdr_bullet_char_1..3` | buffer | str | `•◦▸` | ネスト深さ別箇条書き文字 |
| `mkdr_task_unchecked` | buffer | str | `☐` | 未チェックタスク文字 |
| `mkdr_task_checked` | buffer | str | `☑` | チェック済みタスク文字 |
| `mkdr_code_fence_char` | buffer | str | `▔` | コードフェンス罫線文字 |
| `mkdr_enable_bold` | buffer | bool | `true` | 太字レンダリング有効化 |
| `mkdr_enable_italic` | buffer | bool | `true` | イタリックレンダリング有効化 |
| `mkdr_enable_code_span` | buffer | bool | `true` | インラインコードレンダリング有効化 |
| `mkdr_enable_link` | buffer | bool | `true` | リンクレンダリング有効化 |
| `mkdr_enable_table` | buffer | bool | `true` | テーブルレンダリング有効化 |

---
