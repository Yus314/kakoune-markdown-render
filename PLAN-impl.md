# kakoune-markdown-render — Milestone 1〜5 実装

> 設計概要・アーキテクチャ・パフォーマンス設計は [PLAN.md](PLAN.md) 参照
> 技術的注意事項・フェイス一覧・オプション一覧・M6〜M9 は [PLAN-reference.md](PLAN-reference.md) 参照

---

## Milestone 1 — コアインフラ

**目標**: バイトオフセット変換・Kakoune出力・設定・UDS通信基盤・パス管理

### `src/paths.rs` — ソケットパス管理

```rust
/// FNV-1a ハッシュ（64bit）。クレート全体で使用するため pub(crate)。
/// paths.rs の session_hash()、config.rs の Config::hash()、daemon/state.rs で共有。
pub(crate) fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes { h ^= b as u64; h = h.wrapping_mul(0x100000001b3); }
    h
}

/// セッション名を FNV-1a でハッシュし、先頭16文字の hex 文字列を返す。
/// sockaddr_un.sun_path の長さ制限（~108バイト）を回避するために必須。
fn session_hash(session: &str) -> String {
    format!("{:016x}", fnv1a(session.as_bytes()))
}

/// daemon ソケットのパスを返す。
/// 優先: $XDG_RUNTIME_DIR/mkdr/<hash>/daemon.sock（tmpfs、0700、systemd管理）
/// 次点: /tmp/mkdr-<uid>/<hash>/daemon.sock（0700 で作成）
pub fn socket_path(session: &str) -> PathBuf {
    let hash = session_hash(session);
    let base = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(format!("/tmp/mkdr-{}", get_uid())));
    base.join("mkdr").join(&hash).join("daemon.sock")
}

/// ディレクトリを 0700 で作成（セキュリティ上必須）。
pub fn ensure_session_dir(session: &str) -> anyhow::Result<PathBuf> {
    let path = socket_path(session).parent().unwrap().to_owned();
    std::fs::create_dir_all(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    Ok(path)
}

/// プロセスの実 UID を返す（/tmp フォールバック用）。
/// `libc::getuid()` は POSIX 保証。Linux/macOS/BSD で動作する。
#[cfg(unix)]
fn get_uid() -> u32 {
    // SAFETY: getuid() は常に成功し副作用もない
    unsafe { libc::getuid() }
}
```

### `src/offset.rs`

```rust
/// バッファ内の各行の開始バイトオフセット（0-indexed）
pub fn line_starts(content: &str) -> Vec<usize> {
    let mut s = vec![0];
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' { s.push(i + 1); }
    }
    s
}

/// バイトオフセット → (行, 列) 変換（1-based）
/// pulldown-cmark の Range.end は排他的。Kakoune は包含的。
/// → end 側は byte_to_line_col(starts, range.end - 1) を使うこと。
pub fn byte_to_line_col(starts: &[usize], offset: usize) -> (usize, usize) {
    let line = starts.partition_point(|&s| s <= offset) - 1;
    (line + 1, offset - starts[line] + 1)
}
```

### `src/kak.rs`

```rust
pub struct KakRange {
    pub line_start: usize,
    pub col_start:  usize,
    pub line_end:   usize,   // inclusive（Kakoune 仕様）
    pub col_end:    usize,
    /// `replace-ranges`（conceal）用: markup string（例: `{MkdrBold}▌{/}`）
    /// `ranges`（faces）用: フェイス名（例: `MkdrBold`）
    /// どちらに入れるかは呼び出し側が管理し、KakRange 自体は区別しない。
    pub text: String,
}

impl KakRange {
    /// Kakoune range-specs 形式に変換: `line.col,line.col|text`
    /// conceal 側: text = markup string（`escape_markup()` 適用済みであること）
    /// faces 側:   text = フェイス名（エスケープ不要）
    pub fn to_spec(&self) -> String {
        format!("{}.{},{}.{}|{}",
            self.line_start, self.col_start,
            self.line_end,   self.col_end,
            self.text)
    }
}

pub fn kakquote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

pub fn escape_markup(s: &str) -> String {
    // replace-ranges の markup string では \ | { を全てエスケープ必須。
    // \ は他の置換のプレフィックスになるので最初に処理する。
    // { はリテラルとして表示する場合 \{ が必要（face 切替として誤解釈される）。
    s.replace('\\', "\\\\").replace('|', "\\|").replace('{', "\\{")
}

/// conceal + faces のコマンドを生成。
/// try-client / try-catch でクライアント消滅・バッファ消滅を安全に無視する。
/// timestamp は Kakoune 側で自動検証される。
/// `mkdr_last_timestamp` / `mkdr_last_width` はここで設定する（非オプティミスティック）。
/// `mkdr_last_config_hash` は `"{ts:016x}:{hash:016x}"` 形式で保存し、
/// 次回 PING の世代チェック（fire-and-forget 競合防止）と設定変更検出の両方に使う。
///
/// 出力形式:
/// try %{
///   evaluate-commands -try-client <client> %{
///     evaluate-commands -buffer <bufname> %{
///       set-option window mkdr_conceal <ts> 'spec1' 'spec2' ...
///       set-option window mkdr_faces   <ts> 'spec1' 'spec2' ...
///       set-option window mkdr_last_timestamp <ts>
///       set-option window mkdr_last_width <width>
///       set-option window mkdr_last_config_hash '<ts_hex>:<hash_hex>'
///     }
///   }
/// } catch %{}
///
/// 各 range-spec は kakquote(range.to_spec()) でシングルクォートでラップする。
/// 理由: spec の text フィールド（置換文字列やフェイス名）にスペースや特殊文字が
/// 含まれる可能性があり、クォートなしでは Kakoune のコマンドパーサが誤分割する。
/// 例: '1.1,1.5|{MkdrHeading1}▌ ' の末尾スペースはクォートが必須。
///
/// bufname は kakquote で囲む（パスにシングルクォートを含む場合に対応）。
pub fn format_commands(
    client:      &str,
    bufname:     &str,
    timestamp:   u64,
    width:       usize, // mkdr_last_width の非オプティミスティック更新に使用
    conceal:     &[KakRange],
    faces:       &[KakRange],
    config_hash: u64,  // RENDER/PING 後に mkdr_last_config_hash へ保存する FNV-1a ハッシュ
) -> String { ... }

// `EmitSink` trait と `KakPipeSink` は response.rs に定義する。
// （emit_to_kak を呼ぶ実装が response.rs にあるため、同ファイルに置く方が自然。
//  kak.rs はデータ型とフォーマット関数のみとし、出力先の抽象化は response.rs に担わせる。）
//
// 定義場所まとめ:
//   kak.rs          → KakRange, escape_markup, format_commands（純粋なデータ/フォーマット）
//                      format_commands は (width, config_hash) パラメータを受け取り
//                      mkdr_last_timestamp / mkdr_last_width / mkdr_last_config_hash を出力に含める
//   response.rs     → emit_to_kak（fire-and-forget）, EmitSink, KakPipeSink,
//                      spawn_kak_p_with_timeout（タイムアウト付き kak -p 実行）
//   daemon/mod.rs   → parse_config_hash_str（"{ts:016x}:{hash:016x}" → (u64, u64)）
//   tests/common.rs → RecordSink（テスト用記録シンク）

### `src/config.rs`

```rust
pub struct Config {
    pub cursor_context: usize,
    /// H1〜H6 のプレフィックス文字。**0-indexed**（H1 = heading_char[0]、H6 = heading_char[5]）。
    /// Kakoune オプションは 1-indexed（mkdr_heading_char_1..6）なので from_env() で変換する。
    pub heading_char: [char; 6],
    pub heading_setext: bool,
    pub thematic_char: char,
    pub blockquote_char: char,
    pub bullet_chars: [char; 3],
    pub task_unchecked: char,
    pub task_checked:   char,
    pub code_fence_char: char,
    pub enable_bold:   bool,
    pub enable_italic: bool,
    pub preset: Preset,
    // ...
}

impl Config {
    /// kak_opt_mkdr_* 環境変数から設定を読み込む（mkdr send 側で使用）
    ///
    /// # Unset 規約
    /// 環境変数が空文字列 ("") の場合は「未設定」とみなしデフォルト値を使用する。
    /// Kakoune は `set-option global mkdr_foo ''` で実質的に "unset" できる。
    /// これにより global → buffer のスコープ継承でデフォルト値に戻せる。
    ///
    /// # 実装方針
    /// `from_env_inner(lookup: impl Fn(&str) -> Option<String>) -> Self` を定義し、
    /// `from_env()` は `std::env::var` を渡すラッパーにする。
    /// テスト時は HashMap を渡すことで環境変数なしで任意の設定値を注入できる。
    pub fn from_env() -> Self { ... }

    /// テスト用: key-value ペアから Config を構築（環境変数に依存しない）
    /// `cargo test` の並列実行時に `env::set_var` が他スレッドに干渉する問題を回避する。
    ///
    /// ```rust
    /// let config = Config::from_pairs(&[
    ///     ("kak_opt_mkdr_thematic_char", "="),
    ///     ("kak_opt_mkdr_cursor_context", "2"),
    /// ]);
    /// ```
    #[cfg(test)]
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        Self::from_env_inner(|key| {
            pairs.iter().find(|(k, _)| *k == key).map(|(_, v)| v.to_string())
        })
    }

    /// KEY=VALUE\n 形式のバイト列からパース（daemon 受信時）
    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> { ... }

    /// KEY=VALUE\n 形式にシリアライズ（mkdr send 送信時）
    ///
    /// # フィールド順（変更禁止）
    /// フィールドは必ず **アルファベット順** で出力する。
    /// 順序を変えると `config_hash`（FNV-1a）が変わり、既存の PING キャッシュが全て無効化される。
    /// 新フィールド追加時もアルファベット順を維持すること。
    pub fn to_bytes(&self) -> Vec<u8> { ... }

    /// FNV-1a ハッシュ（PING の config_hash フィールド用）
    pub fn hash(&self) -> u64 {
        fnv1a(&self.to_bytes())
    }
}
```

### `src/send.rs`

```rust
pub fn run_send(args: &SendArgs) -> anyhow::Result<()> {
    let sock = socket_path(&args.session);

    // --check-alive: ソケット接続できれば exit 0、できなければ exit 1
    // 何も送信しない（daemon 側は EOF を受けて parse_message がエラー → eprintln のみ）
    if args.check_alive {
        return UnixStream::connect(&sock)
            .map(|_| ())
            .with_context(|| format!("daemon not running: {}", sock.display()));
    }

    let mut stream = UnixStream::connect(&sock)
        .with_context(|| format!("daemon not running? {}", sock.display()))?;

    // タイムアウト設定（daemon 無応答時の kak フリーズを防止）
    // 400ms を超えたら EPIPE 相当のエラーとして扱い、stdin drain して終了
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
        // config_hash_str: kak_opt_mkdr_last_config_hash の値をそのまま転送する。
        // 形式は "{ts:016x}:{hash:016x}"。daemon が parse_config_hash_str() で検証する。
        // 空文字列（初回 PING、GlobalSetOption リセット後）は daemon が即スキップ（RENDER 待ち）。
        // send.rs 側ではパース不要: 文字列を生のまま PING メッセージに埋め込むだけ。
        let config_hash_str = args.config_hash.as_deref().unwrap_or("");
        writeln!(stream, "PING\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            args.session, args.bufname.as_deref().unwrap_or(""),
            args.timestamp.unwrap_or(0), args.cursor.unwrap_or(0), args.width.unwrap_or(0),
            config_hash_str, args.client.as_deref().unwrap_or(""),
            args.cmd_fifo.as_deref().unwrap_or(""))?;
    } else {
        // Config を環境変数から生成
        let config = Config::from_env().to_bytes();
        writeln!(stream, "RENDER\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            args.session, args.bufname, args.timestamp,
            args.cursor, args.width, args.client, args.cmd_fifo,
            config.len())?;
        stream.write_all(&config)?;

        // Content は stdin から直接 UDS へストリーム（read_to_end 不使用）
        // 書き込みエラー（BrokenPipe/TimedOut/WouldBlock 等）が起きても
        // stdin を必ず drain し kak_response_fifo のライターブロックを解放する。
        // drain しないと %sh{} の親プロセスが response_fifo の write 待ちでハングする。
        if let Err(_) = io::copy(&mut io::stdin(), &mut stream) {
            io::copy(&mut io::stdin(), &mut io::sink()).ok();
        }
    }
    Ok(())
}
```

---

## Milestone 2 — ブロック要素レンダリング

**目標**: 主要ブロック要素の `KakRange` 生成

各要素モジュールは以下のシグネチャに従う:

```rust
/// 共通コンテキスト（render/mod.rs 内で組み立て、各モジュールに渡す）
/// event は含めない。ディスパッチ元（render_unfiltered）が既にイベント種別を知っており、
/// 各モジュールに event を渡しても型情報が失われた &Event として届くだけで有用でない。
/// モジュール固有の追加情報（level, checked, window_width 等）は個別の引数で渡す。
pub struct RenderCtx<'a> {
    pub content:      &'a str,
    pub starts:       &'a [usize],
    pub config:       &'a Config,
    pub window_width: usize,
}

/// 基本シグネチャ（固有情報なしのモジュール: blockquote, strikethrough 等）
pub fn render(
    range:   std::ops::Range<usize>,
    ctx:     &RenderCtx<'_>,
    conceal: &mut Vec<KakRange>,
    faces:   &mut Vec<KakRange>,
);

/// 固有情報あり（各モジュールの実際のシグネチャ）
// heading::render(range, level: usize, ctx, conceal, faces)
// list::render(range, depth: usize, is_ordered: bool, start_num: u64, ctx, conceal, faces)
// task::render(range, checked: bool, ctx, conceal, faces)
// thematic/code_block: window_width は ctx.window_width から取得するため追加引数不要
```

### pulldown-cmark GFM オプション（必須）

```rust
let opts = Options::ENABLE_TABLES
    | Options::ENABLE_TASKLISTS
    | Options::ENABLE_STRIKETHROUGH
    | Options::ENABLE_SMART_PUNCTUATION;
let parser = Parser::new_ext(content, opts).into_offset_iter();
```

### `src/render/thematic.rs` — テーマ区切り

- `Event::Rule` の range は `---` / `***` / `___` を含む**行全体**（改行の手前まで）
  → range.start〜range.end-1 を `window_width` 分の `config.thematic_char` で置換（conceal）
- `MkdrThematicBreak` フェイスを同 range に適用

### `src/render/heading.rs` — ATX見出し

- `Start(Tag::Heading { level, .. })` の range は `# title ###` 行全体（改行手前まで）
- プレフィックスはソースから実測（`#+ ` を走査、closing `###` 等の揺れに対応）
- 実測バイト範囲を `config.heading_char[level - 1]`（0-indexed）+ ` ` に置換（conceal）
- setext形式は Milestone 8 で対応

### `src/render/blockquote.rs` — 引用ブロック

- `Start(Tag::BlockQuote(_))` は**ネスト階層ごとに独立して発火**する
  （`> > text` では外側と内側の BlockQuote が別々の Start/End ペアを持つ）
- 各 Start で range の先頭から `>` マーカーをソースで実測し `config.blockquote_char` + ` ` に置換
- ネスト2段の場合、2つのマーカー（`> >`）がそれぞれ独立して置換される

### `src/render/list.rs` — リスト

- 順不同リスト: `Start(Tag::Item)` の range 先頭から `-` / `*` / `+` をソースで実測し
  `config.bullet_chars[depth % 3]` に置換（conceal）
- 順序付きリスト: `1. ` のうち **`1.`**（数字+ピリオド）に `MkdrOrderedList` フェイス適用
  （trailing スペースはフェイス対象外。`start` 番号はソースから実測）

**ネスト深さの取得方法**:
pulldown-cmark の `Start(Tag::Item)` イベントは直接ネスト深さを提供しない。
`render_unfiltered()` のイベントループ内で `Start(Tag::List(...))` / `End(Tag::List(...))` を
カウントするスタック（depth カウンタ）を維持する。

```rust
// render/mod.rs の render_unfiltered() 内
let mut list_depth: usize = 0;

for (event, range) in parser {
    match &event {
        Event::Start(Tag::List(_)) => list_depth += 1,
        Event::End(Tag::List(_))   => list_depth = list_depth.saturating_sub(1),
        Event::Start(Tag::Item)    =>
            list::render(range, list_depth.saturating_sub(1), ..., &ctx, conceal, faces),
        ...
    }
}
```

`Tag::List(None)` = 順不同、`Tag::List(Some(n))` = 順序付き（start 番号 n）。
`list::render` はこの情報を `is_ordered: bool` と `start_num: u64` として受け取る。
これは `render_unfiltered()` がブロック要素で「Start のみ処理」という方針の例外だが、
リストのネスト深さを正確に得るには `List` のネスト追跡が必須。

### `src/render/task.rs` — タスクリストマーカー

- pulldown-cmark の range（`[ ]` または `[x]` の3バイト）は信頼できる
- 3バイトを `config.task_unchecked` / `config.task_checked` に置換（conceal）
- チェック状態に応じて `MkdrTaskUnchecked` / `MkdrTaskChecked` フェイスを同 range に適用

### `src/render/code_block.rs` — フェンスコードブロック

- `Start(Tag::CodeBlock(_))` の range はフェンス開始行〜フェンス終了行の全体
- フェンス文字数（`` ``` `` / `~~~` の長さ）はソースから実測してフェンス行を border 文字に置換
- 言語ラベル行はソースから手動パースして `MkdrCodeLang` フェイス適用

### `src/render/strikethrough.rs` — 打ち消し線（M2 で実装）

- `Start(Tag::Strikethrough)` の range は `~~text~~` 全体
- `~~` マーカー（2バイト固定）を空文字列に置換（conceal）
- テキスト部分に `MkdrStrikethrough` フェイス適用

### `src/render/mod.rs` — メインウォーク

```rust
pub struct Renderer<'a> {
    content:      &'a str,
    starts:       Vec<usize>,   // line_starts(content) で事前計算
    config:       &'a Config,
    // cursor_line は持たない。フィルタは filter_cursor_overlap() で外部から掛ける。
    window_width: usize,
}

impl<'a> Renderer<'a> {
    pub fn new(content: &'a str, config: &'a Config, window_width: usize) -> Self {
        Renderer {
            content,
            starts: line_starts(content),  // src/offset.rs の関数
            config,
            window_width,
        }
    }

    /// 未フィルタの KakRange を返す。フィルタは emit 直前に apply する。
    pub fn render_unfiltered(&self) -> (Vec<KakRange>, Vec<KakRange>) {
        let mut conceal: Vec<KakRange> = Vec::new();
        let mut faces:   Vec<KakRange> = Vec::new();

        let ctx = RenderCtx {
            content:      self.content,
            starts:       &self.starts,
            config:       self.config,
            window_width: self.window_width,
        };

        let opts = Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS
                 | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_SMART_PUNCTUATION;

        // into_offset_iter() はイベントごとにソース上のバイト Range を返す。
        // ブロック要素は Start イベントの range だけで処理できる。
        // 例外: リストのネスト深さは List Start/End のカウンタで追跡が必要。
        let mut list_depth: usize = 0;
        let mut list_is_ordered = false;
        let mut list_start_num: u64 = 1;

        for (event, range) in Parser::new_ext(self.content, opts).into_offset_iter() {
            match &event {
                Event::Start(Tag::List(start)) => {
                    list_depth += 1;
                    list_is_ordered = start.is_some();
                    list_start_num = start.unwrap_or(1);
                }
                Event::End(Tag::List(_)) => {
                    list_depth = list_depth.saturating_sub(1);
                }

                Event::Start(Tag::Heading { level, .. }) =>
                    heading::render(range, *level as usize, &ctx, &mut conceal, &mut faces),

                Event::Rule =>
                    thematic::render(range, &ctx, &mut conceal, &mut faces),

                Event::Start(Tag::BlockQuote(_)) =>
                    blockquote::render(range, &ctx, &mut conceal, &mut faces),

                Event::Start(Tag::Item) =>
                    list::render(range,
                        list_depth.saturating_sub(1),  // depth は 0-indexed
                        list_is_ordered, list_start_num,
                        &ctx, &mut conceal, &mut faces),

                Event::TaskListMarker(checked) =>
                    task::render(range, *checked, &ctx, &mut conceal, &mut faces),

                Event::Start(Tag::CodeBlock(_)) =>
                    code_block::render(range, &ctx, &mut conceal, &mut faces),

                Event::Start(Tag::Strikethrough) =>
                    strikethrough::render(range, &ctx, &mut conceal, &mut faces),

                // M7 以降: emphasis/strong/code_span/link は別途追加
                _ => {}
            }
        }
        (conceal, faces)
    }
}

/// emit 直前にカーソル近傍を除外する（line_start/line_end 両端で重なり判定）
///
/// context=0 の場合は `Cow::Borrowed` でゼロコピー返却（Vec 確保なし）。
/// context>0 の場合は `Cow::Owned` でフィルタ済み新 Vec を返す。
/// これにより大多数のケース（cursor_context=0 のデフォルト）で余分な Vec 確保を回避する。
pub fn filter_cursor_overlap<'a>(
    ranges: &'a [KakRange],
    cursor_line: usize,
    context: usize,
) -> Cow<'a, [KakRange]> {
    // context=0 はフィルタなし（全 range を通す）。
    // Cow::Borrowed でゼロコピー返却。
    // 早期 return がないと lo=cursor_line, hi=cursor_line となり
    // cursor_line を含む range が全て除外される（機能バグ）。
    if context == 0 { return Cow::Borrowed(ranges); }
    let lo = cursor_line.saturating_sub(context);
    let hi = cursor_line + context;
    Cow::Owned(
        ranges.iter()
            .filter(|r| r.line_end < lo || r.line_start > hi)
            .cloned()
            .collect()
    )
}
// Cow を使うために use std::borrow::Cow; を render/mod.rs の先頭に追加すること
```

### フェイス・range 重なりポリシー

- 内側（より具体的）な range を後に追加することで内側を優先
- 太字+イタリック重複範囲は `MkdrBoldItalic` を単一エントリとして追加

---

## Milestone 3 — バイナリ完成

**目標**: `mkdr daemon` と `mkdr send` の両サブコマンドが動作する

> **M3 と M5 の分担**
> - **M3**: シングルスレッドの基本 daemon（accept → 即 render → emit、コアレスシングなし）
> - **M5**: 2スレッド化 + mpsc channel + コアレスシングに置き換える
>
> M3 の daemon は `mod.rs` で `for stream in listener.incoming()` をループし
> 接続ごとに `parse_message → handle_render/ping/close/shutdown` を呼ぶシンプル実装。
> M4 の kak プラグインと組み合わせて基本動作を確認した後、M5 で性能向上を図る。

### `src/main.rs` — CLI

```rust
#[derive(Parser)]
enum Cli {
    /// デーモン起動（UDS で待ち受け、2スレッド構成）
    Daemon { #[arg(long)] session: String },

    /// daemon に PING または RENDER を送信
    Send {
        // モードに関わらず必須
        #[arg(long)] session: String,

        // PING / RENDER / CLOSE で必須（check_alive / shutdown では不要）
        #[arg(long)] bufname:   Option<String>,
        // PING / RENDER でのみ必須
        #[arg(long)] timestamp: Option<u64>,
        #[arg(long)] cursor:    Option<usize>,
        #[arg(long)] width:     Option<usize>,
        #[arg(long)] client:    Option<String>,
        #[arg(long)] cmd_fifo:  Option<String>,

        /// PING モード（bufname/timestamp/cursor/width/client/cmd_fifo 必須）
        #[arg(long)] ping: bool,
        /// 接続確認のみ（session のみ必須。成否を終了コードで返す）
        #[arg(long)] check_alive: bool,
        /// バッファ状態を daemon から解放（session + bufname 必須）
        #[arg(long)] close: bool,
        /// daemon を停止（session のみ必須）
        #[arg(long)] shutdown: bool,

        /// PING 時の config キャッシュ文字列（`"{ts:016x}:{hash:016x}"` 形式）。
        /// kak 側の mkdr_last_config_hash から渡す。send.rs はパースせずそのまま転送する。
        /// 空文字列の場合は daemon が PING をスキップする（RENDER 待ち）。
        #[arg(long)] config_hash: Option<String>,
    },
}
```

### M3 シングルスレッド daemon（`src/daemon/mod.rs`）

M3 時点では accept と render を単一スレッドで処理する基本実装：

```rust
/// M3 版: シングルスレッド（M5 で2スレッド+コアレスシングに置き換え）
pub fn run(session: &str) -> anyhow::Result<()> {
    run_with_sink(session, KakPipeSink)
}

/// テスト用エントリポイント: sink を差し替えることで kak なし統合テストが可能
pub fn run_with_sink(session: &str, mut sink: impl EmitSink) -> anyhow::Result<()> {
    // ソケットの親ディレクトリを 0700 で作成（ensure_session_dir 参照）
    ensure_session_dir(session)?;
    let sock_path = socket_path(session);
    // 起動時に残留ソケットを削除してから bind する。
    // 二重起動は bind() の失敗（EADDRINUSE）で自然に防止される。
    // 先に起動した daemon が生きているなら --check-alive で検知済みのため
    // ここに来るのは残留ソケットのみのケース。
    let _ = std::fs::remove_file(&sock_path);
    let listener = UnixListener::bind(&sock_path)?;
    let mut state = SessionState::default();

    for stream in listener.incoming() {
        match parse_message(BufReader::new(stream?)) {
            Ok(Message::Render(r))   => handle_render(r, &mut state, &mut sink),
            Ok(Message::Ping(p))     => handle_ping(p, &mut state, &mut sink),
            Ok(Message::Close(c))    => state.remove_buf(&c.bufname),
            Ok(Message::Shutdown(_)) => break,
            // UnexpectedEof = --check-alive 接続（何も送らず即クローズ）: 正常
            Err(ref e) if matches!(e.downcast_ref::<io::Error>(), Some(e) if e.kind() == io::ErrorKind::UnexpectedEof) => {}
            Err(e) => eprintln!("parse error: {e}"),
        }
    }
    let _ = std::fs::remove_file(&sock_path);
    Ok(())
}
```

### 統合テスト

アサーション形式: Rust の `#[test]` で `Renderer::render_unfiltered()` を呼び、
返り値の `(Vec<KakRange>, Vec<KakRange>)` をインラインの期待値と比較する。

```rust
// 例: tests/heading_test.rs
#[test]
fn heading_h1_basic() {
    let content = "# Hello World\n";
    let config = Config::default();
    let renderer = Renderer::new(content, &config, 80);
    let (conceal, faces) = renderer.render_unfiltered();
    // conceal: `# ` (2バイト) が heading_char[0] + ` ` に置換される
    assert_eq!(conceal.len(), 1);
    assert_eq!(conceal[0].line_start, 1);
    assert_eq!(conceal[0].col_start,  1);
    assert_eq!(conceal[0].col_end,    2);  // `#` + ` ` の2バイト
    // faces: 行全体に MkdrHeading1 フェイス
    assert_eq!(faces[0].text, "MkdrHeading1");
}
```

テストファイル:
```
tests/
├── heading_test.rs       # H1〜H6、closing ###、スペース揺れ、UTF-8マルチバイト
├── thematic_test.rs      # --- *** ___ パターン、幅別の文字数
├── blockquote_test.rs    # ネスト引用、インデントあり、スペースなし >
├── list_test.rs          # 順不同（-/*/ +）・順序付き・ネスト・タスク
├── code_fence_test.rs    # 言語あり・なし、~~~形式、フェンス長4以上
├── strikethrough_test.rs # ~~text~~ 基本動作
├── offset_test.rs        # byte_to_line_col のエッジケース（後述）
└── kak_test.rs           # escape_markup + format_commands ユニットテスト（後述）
```

各テストは `.md` 文字列をインラインで定義し、外部ファイルへの依存を排除する。

### `byte_to_line_col` エッジケーステスト（`tests/offset_test.rs`）

```rust
#[test]
fn empty_file() {
    let starts = line_starts("");
    assert_eq!(starts, vec![0]);
    // 空ファイルに range.end=0 は来ないが starts[0]=0 が正しく存在する
}

#[test]
fn no_trailing_newline() {
    let content = "abc";
    let starts = line_starts(content);
    assert_eq!(starts, vec![0]);
    // "abc" の最後のバイト index=2、range.end-1=2 → col=3
    assert_eq!(byte_to_line_col(&starts, 2), (1, 3));
}

#[test]
fn multibyte_utf8() {
    // "日" は 3 バイト（UTF-8: E6 97 A5）
    let content = "日本語\n";
    let starts = line_starts(content);
    assert_eq!(starts, vec![0, 10]);  // "日本語\n" = 9 バイト + \n = 10
    // バイトオフセット 8 = 最後の "語" の末尾-1
    assert_eq!(byte_to_line_col(&starts, 8), (1, 9));
}

#[test]
fn range_end_exclusive_to_inclusive() {
    // pulldown-cmark の range.end は排他的 → range.end-1 で包含的 end を求める
    let content = "# Hello\n";
    let starts = line_starts(content);
    // range.end = 8（\n の次）→ range.end-1 = 7（\n）
    // Kakoune の end は行末の実際の文字位置なので \n 手前: col=7
    assert_eq!(byte_to_line_col(&starts, 7), (1, 8));
}
```

`line_starts` と `byte_to_line_col` は `pub(crate)` として `src/offset.rs`（または `src/render/mod.rs`）に定義し、テストから直接呼べるようにする。

### `escape_markup` + `format_commands` テスト（`tests/kak_test.rs`）

```rust
// escape_markup のユニットテスト（純粋関数 → テスト容易）
#[test]
fn escape_backslash_first() {
    // \ を最初に処理しないと \ → \\ した後の | → \| が \\ + | → \\| になる（二重エスケープバグ）
    assert_eq!(escape_markup("a\\|b"), "a\\\\\\|b");
}
#[test] fn escape_pipe()   { assert_eq!(escape_markup("a|b"), "a\\|b"); }
#[test] fn escape_brace()  { assert_eq!(escape_markup("a{b"), "a\\{b"); }
#[test] fn escape_empty()  { assert_eq!(escape_markup(""), ""); }
#[test] fn escape_japanese() {
    // マルチバイト文字はエスケープ不要
    assert_eq!(escape_markup("日本語"), "日本語");
}

// format_commands の出力形式テスト
#[test]
fn format_commands_empty() {
    let s = format_commands("client1", "buf1", 42, 80, &[], &[], 0xdeadbeef);
    assert!(s.contains("set-option window mkdr_conceal 42"));
    assert!(s.contains("set-option window mkdr_faces   42"));
    assert!(s.contains("set-option window mkdr_last_timestamp 42"));
    assert!(s.contains("set-option window mkdr_last_width 80"));
    assert!(s.contains("set-option window mkdr_last_config_hash"));
    assert!(s.contains("evaluate-commands -try-client client1"));
    assert!(s.contains("-buffer buf1"));
}

#[test]
fn format_commands_try_catch_wrapping() {
    let s = format_commands("c", "b", 1, 80, &[], &[], 0);
    // クライアント消滅・バッファ消滅への対策として try/catch でラップされている
    assert!(s.starts_with("try %{"));
    assert!(s.ends_with("} catch %{}"));
}

#[test]
fn format_commands_config_hash_format() {
    let s = format_commands("c", "b", 42, 80, &[], &[], 0xdeadbeef12345678);
    // "{ts:016x}:{hash:016x}" 形式で保存される
    assert!(s.contains("mkdr_last_config_hash '000000000000002a:deadbeef12345678'"));
}
```

### テスト分類ポリシー

| 種別 | 場所 | 対象 |
|------|------|------|
| **ユニットテスト** | `src/` 内の `#[cfg(test)]` | `fnv1a`、`Config::from_bytes`、`Config::hash` |
| **統合テスト** | `tests/` ディレクトリ | `render_unfiltered`、`filter_cursor_overlap`、`byte_to_line_col`、`escape_markup`、`format_commands`、daemon 起動を含むシナリオ |
| **ベンチマーク** | `benches/` | `render_unfiltered`（大量行）、`format_commands`（大量 range）、`filter_cursor_overlap` |
| **手動テスト** | 検証ポイント 1 / 2 | kak プラグイン（.kak スクリプト）、エンドツーエンド UI 確認 |

**kak プラグインの自動テスト方針**: `.kak` スクリプトの自動テストは実装しない。
`kak -f` によるヘッドレステストは複雑すぎてコスト対効果が低いため、代わりに:
- 検証ポイント 1（M4）の手動確認リストを実際の Kakoune 上で実施
- `%sh{}` 内のロジックはできるだけ Rust 側（`mkdr send`）に寄せて kak スクリプトを薄くする
- 重要な不変条件は kak スクリプト内コメントで明記（B-3/B-4 修正事項等）

### `filter_cursor_overlap` 境界値テスト

| ケース | cursor_line | context | 期待動作 |
|--------|------------|---------|---------|
| context=0 | 任意 | 0 | 全 range を通す（early-return 必須。なければ cursor_line を含む range が除外される機能バグ） |
| context=0, cursor_line=1 | 1 | 0 | early-return → 全 range を通す |
| context=1, cursor_line=0 | 0 | 1 | saturating_sub(1)=0、hi=1: line 0..1 を除外（usize アンダーフローなし） |
| 単一行 range、ちょうど境界 | 5 | 2 | lo=3, hi=7: line 3..7 が除外される |
| 複数行 range の line_start < lo だが line_end = lo | 5 | 2 | line_end=3 (=lo) → 除外される（line_end < lo は偽） |
| 複数行 range の line_start = hi | 5 | 2 | line_start=7 (=hi) → 除外される（line_start > hi は偽） |
| 複数行 range が境界に接する（line_end = lo-1） | 5 | 2 | line_end=2 < lo=3 → 通過 |
| 複数行 range が境界に接する（line_start = hi+1） | 5 | 2 | line_start=8 > hi=7 → 通過 |

### `Config::hash()` 安定性テスト

```rust
// src/config.rs の #[cfg(test)] モジュール内
#[test]
fn config_hash_stable() {
    let c = Config::default();
    // 同じ設定なら何度呼んでも同じ hash
    assert_eq!(c.hash(), c.hash());
}

#[test]
fn config_hash_changes_on_field() {
    let c1 = Config::default();
    let c2 = Config { thematic_char: '=', ..Config::default() };
    // フィールド値が1つでも変わると hash が変わる
    assert_ne!(c1.hash(), c2.hash());
}

#[test]
fn config_to_bytes_alphabetical() {
    let bytes = Config::default().to_bytes();
    let s = String::from_utf8(bytes).unwrap();
    let keys: Vec<&str> = s.lines()
        .filter_map(|l| l.split('=').next())
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    // フィールド出力順がアルファベット順でなければ hash が不安定になる
    assert_eq!(keys, sorted, "to_bytes() field order must be alphabetical");
}
```

---

## Milestone 4 — kak プラグイン

**目標**: 実際の Kakoune 上で動作確認できる状態

### `rc/options.kak`

```kak
# ハイライター用（window スコープ）
declare-option -hidden range-specs mkdr_conceal
declare-option -hidden range-specs mkdr_faces

# no-op 判定用キャッシュ（window スコープ: 3要素）
declare-option -hidden str mkdr_last_timestamp   ''
declare-option -hidden str mkdr_last_width        ''
declare-option -hidden str mkdr_last_cursor_line  ''

# パフォーマンス最適化キャッシュ（window スコープ）
# mkdr_daemon_alive: true のとき PING パスで --check-alive をスキップ（2-5ms 節約）
#   RENDER パスは常に --check-alive を実行（daemon 死亡検知・自動復旧に使用）
# mkdr_last_config_hash: 最後の RENDER/PING 応答時の "{ts_hex}:{hash_hex}" 複合文字列
#   ts_hex  = 前回 RENDER タイムスタンプ（fire-and-forget 競合防止のための世代識別子）
#   hash_hex = config FNV-1a ハッシュ（設定変更検出）
#   PING 時に --config-hash として渡し、daemon が parse_config_hash_str() で検証する。
#   空文字列 → daemon は即スキップ（RENDER 完了前 / GlobalSetOption リセット後）
declare-option -hidden bool mkdr_daemon_alive     false
declare-option -hidden str  mkdr_last_config_hash ''

# 設定オプション（buffer スコープ、21個）
# heading_char_1〜6: Nerd Font nf-md-numeric_N_circle_outline (U+F0CA1〜U+F0CAB)
declare-option -hidden int  mkdr_cursor_context  0
declare-option -hidden str  mkdr_heading_char_1  '󰲡'
declare-option -hidden str  mkdr_heading_char_2  '󰲣'
declare-option -hidden str  mkdr_heading_char_3  '󰲥'
declare-option -hidden str  mkdr_heading_char_4  '󰲧'
declare-option -hidden str  mkdr_heading_char_5  '󰲩'
declare-option -hidden str  mkdr_heading_char_6  '󰲫'
declare-option -hidden bool mkdr_heading_setext  false
declare-option -hidden str  mkdr_thematic_char   '─'
declare-option -hidden str  mkdr_blockquote_char '▎'
declare-option -hidden str  mkdr_bullet_char_1   '•'
declare-option -hidden str  mkdr_bullet_char_2   '◦'
declare-option -hidden str  mkdr_bullet_char_3   '▸'
declare-option -hidden str  mkdr_task_unchecked  '☐'
declare-option -hidden str  mkdr_task_checked    '☑'
declare-option -hidden str  mkdr_code_fence_char '▔'
declare-option -hidden bool mkdr_enable_bold      true
declare-option -hidden bool mkdr_enable_italic    true
declare-option -hidden bool mkdr_enable_code_span true
declare-option -hidden bool mkdr_enable_link      true
declare-option -hidden bool mkdr_enable_table     true
declare-option -hidden str  mkdr_preset           'default'
```

### `rc/faces.kak`

```kak
set-face global MkdrHeading1      'default+b'
set-face global MkdrHeading2      'default+b'
set-face global MkdrHeading3      'default+bi'
set-face global MkdrHeading4      'default+i'
set-face global MkdrHeading5      'default'
set-face global MkdrHeading6      'default'
set-face global MkdrBold          'default+b'
set-face global MkdrItalic        'default+i'
set-face global MkdrBoldItalic    'default+bi'
set-face global MkdrCode          'default'
set-face global MkdrCodeBlock     'default'
set-face global MkdrBlockQuote    'default+i'
set-face global MkdrThematicBreak 'default'
set-face global MkdrTaskChecked   'green'
set-face global MkdrTaskUnchecked 'default'
set-face global MkdrLink          'blue+u'
set-face global MkdrLinkUrl       'default'
# ...
```

### `rc/commands.kak`

```kak
define-command mkdr-enable -docstring 'Enable markdown rendering' %{
    # try で囲む: WinSetOption が2回発火した場合に add-highlighter が
    # 「既に存在する」エラーで失敗しないようにする
    try %{ add-highlighter window/mkdr-conceal replace-ranges mkdr_conceal }
    try %{ add-highlighter window/mkdr-faces   ranges         mkdr_faces }
    hook -group mkdr window NormalIdle .* mkdr-render
    hook -group mkdr window InsertIdle .* mkdr-render
    hook -group mkdr window WinResize   .* mkdr-on-resize
}

define-command mkdr-disable -docstring 'Disable markdown rendering' %{
    remove-highlighter window/mkdr-conceal
    remove-highlighter window/mkdr-faces
    remove-hooks window mkdr
    # daemon のバッファ状態を解放（メモリリーク防止）
    # evaluate-commands を使う（nop %sh{} でも動作するが意図を明確にするため）
    evaluate-commands %sh{ mkdr send --close --session "$kak_session" --bufname "$kak_bufname" 2>/dev/null & }
}
```

### `rc/markdown-render.kak`

```kak
hook global WinSetOption filetype=markdown %{
    mkdr-enable
    hook -once -always window WinSetOption filetype=(?!markdown).* %{
        mkdr-disable
    }
}

# 幅変化時は強制 RENDER（thematic/code fence が幅依存のため）
# mkdr_last_timestamp も '' にリセットすることで ts_same=0 → RENDER パスを強制する。
# last_width だけリセットすると ts_same=1 AND w_same=0 → PING パスに入り、
# 幅依存の置換（thematic/code fence）が更新されないバグになる。
define-command -hidden mkdr-on-resize %{
    set-option window mkdr_last_width ''
    set-option window mkdr_last_timestamp ''
    mkdr-render
}

define-command -hidden mkdr-render %{
    evaluate-commands %sh{
        # ---- no-op 判定（常に必要な変数のみ参照・export）----
        # 【重要】Kakoune は %sh{} 内で明示参照した変数のみを shell に export する。
        # PING パスでは mkdr_cursor_context / mkdr_daemon_alive / mkdr_last_config_hash の
        # 3オプションのみ参照し、21個の config オプションは RENDER パスにのみ記述する。
        # → PING 発火時に 21 変数の環境変数 export を省略できる。
        # shellcheck disable=SC2034  # 意図的な no-op 参照
        : "${kak_opt_mkdr_cursor_context}" \
          "${kak_opt_mkdr_daemon_alive}" \
          "${kak_opt_mkdr_last_config_hash}"

        ts="$kak_timestamp"
        # kak_window_width は Kakoune 2022 年頃以降で利用可能。
        # 古い Kakoune / 未定義の場合は 80 にフォールバック（clap の parse エラーを防止）。
        # 最低要件: Kakoune 2021.11.08 以降（window_width が導入されたバージョン）
        w="${kak_window_width:-80}"
        cl="$kak_cursor_line"
        last_ts="$kak_opt_mkdr_last_timestamp"
        last_w="$kak_opt_mkdr_last_width"
        last_cl="$kak_opt_mkdr_last_cursor_line"
        ctx="$kak_opt_mkdr_cursor_context"

        ts_same=0; w_same=0; cl_same=0
        [ "$ts" = "$last_ts" ] && ts_same=1
        [ "$w"  = "$last_w"  ] && w_same=1
        [ "$cl" = "$last_cl" ] && cl_same=1

        if [ "$ts_same" = "1" ] && [ "$w_same" = "1" ]; then
            if [ "$ctx" = "0" ] || [ "$cl_same" = "1" ]; then
                # 3要素全て変化なし → 完全 no-op
                exit 0
            fi
        fi

        # ---- IPC 処理 ----
        cmd_fifo="$kak_command_fifo"

        if [ "$ts_same" = "1" ]; then
            # ---- PING パス: カーソル or 幅変化のみ ----
            # mkdr_daemon_alive=true のとき --check-alive をスキップ（2-5ms 節約）。
            # daemon が死亡していても PING は background で失敗するだけ（silent）。
            # 復旧は次回 RENDER パス（ts_same=0）での --check-alive が担う。
            if [ "$kak_opt_mkdr_daemon_alive" != "true" ]; then
                # daemon がまだ起動していない状態で PING は意味がない。
                # mkdr_last_timestamp をリセットして次の Idle で RENDER を強制する。
                printf 'set-option window mkdr_last_timestamp ""\n'
                printf 'set-option window mkdr_last_cursor_line %s\n' "$cl"
                exit 0
            fi
            # --config-hash: mkdr_last_config_hash の "{ts}:{hash}" 複合文字列をそのまま渡す。
            # send.rs はパースせず転送するだけ。daemon が parse_config_hash_str() で検証。
            # 空の場合（RENDER 前や GlobalSetOption リセット後）は daemon が即スキップ（正常）。
            mkdr send --ping \
                --session "$kak_session" --bufname "$kak_bufname" \
                --timestamp "$ts" --cursor "$cl" --width "$w" \
                --client "$kak_client" --cmd-fifo "$cmd_fifo" \
                --config-hash "$kak_opt_mkdr_last_config_hash" \
                >/dev/null 2>&1 &
        else
            # ---- RENDER パス: バッファ変更 ----
            # 21個の config オプションをここで参照（RENDER 時のみ export が必要）。
            # shellcheck disable=SC2034
            : "${kak_opt_mkdr_heading_char_1}" "${kak_opt_mkdr_heading_char_2}" \
              "${kak_opt_mkdr_heading_char_3}" "${kak_opt_mkdr_heading_char_4}" \
              "${kak_opt_mkdr_heading_char_5}" "${kak_opt_mkdr_heading_char_6}" \
              "${kak_opt_mkdr_thematic_char}"  "${kak_opt_mkdr_blockquote_char}" \
              "${kak_opt_mkdr_bullet_char_1}"  "${kak_opt_mkdr_bullet_char_2}" \
              "${kak_opt_mkdr_bullet_char_3}"  "${kak_opt_mkdr_task_unchecked}" \
              "${kak_opt_mkdr_task_checked}"   "${kak_opt_mkdr_code_fence_char}" \
              "${kak_opt_mkdr_enable_bold}"    "${kak_opt_mkdr_enable_italic}" \
              "${kak_opt_mkdr_enable_code_span}" "${kak_opt_mkdr_enable_link}" \
              "${kak_opt_mkdr_enable_table}"   "${kak_opt_mkdr_preset}"

            # daemon 起動確認: RENDER パスは常に --check-alive（daemon 死亡検知・復旧）。
            # 二重起動の競合（複数 markdown バッファを同時に開いた場合）は
            # mkdr daemon 内の bind() の原子性で解決する:
            #   - 先に bind() した daemon が勝ち → ソケット作成成功
            #   - 後から bind() した daemon は EADDRINUSE で失敗して即終了
            if ! mkdr send --check-alive --session "$kak_session" 2>/dev/null; then
                mkdr daemon --session "$kak_session" >/dev/null 2>&1 &
                # ソケット作成を待つ（最大0.5秒）
                # 注: sleep 0.05 は GNU coreutils 依存（BSD では sleep 1 等に調整が必要）
                for _ in 1 2 3 4 5 6 7 8 9 10; do
                    mkdr send --check-alive --session "$kak_session" 2>/dev/null && break
                    sleep 0.05
                done
            fi
            # daemon 生存を window にキャッシュ（次の PING パスで --check-alive をスキップ）
            printf 'set-option window mkdr_daemon_alive true\n'

            response_fifo="$kak_response_fifo"
            # kak_response_fifo は Kakoune が生成するパスのため、シングルクォートは含まれない。
            # sed による quote エスケープ処理は不要（sed プロセス起動コストを排除）。
            printf "eval -no-hooks write '%s'\n" "$response_fifo" > "$cmd_fifo"
            (
                trap - INT QUIT
                mkdr send \
                    --session "$kak_session" --bufname "$kak_bufname" \
                    --timestamp "$ts" --cursor "$cl" --width "$w" \
                    --client "$kak_client" --cmd-fifo "$cmd_fifo" \
                    < "$response_fifo" >/dev/null 2>&1
            ) >/dev/null 2>&1 &
            # mkdr_last_timestamp / mkdr_last_width / mkdr_last_config_hash は
            # daemon の kak -p 応答（format_commands 出力）で設定される（非オプティミスティック）。
            # ここで設定すると RENDER が daemon 側で失敗しても ts がキャッシュされ、
            # 以降の PING が全て空振りして描画が永久に更新されなくなるバグになる。
        fi
        # カーソル行キャッシュは常に更新（PING/RENDER 問わず）
        printf 'set-option window mkdr_last_cursor_line %s\n' "$cl"
    }
}

# 設定変更時: キャッシュをリセットして次回 RENDER を強制
# mkdr_last_config_hash もリセット（config が変わったので PING のキャッシュ hash は無効）
# mkdr_daemon_alive はリセットしない（設定変更で daemon は死なない）
# 注: このフックは global スコープの option 変更のみ検知。buffer スコープの変更は
#   BufSetOption フックが必要（M9 TODO: BufSetOption mkdr_.* によるキャッシュリセット）
hook global GlobalSetOption mkdr_.* %{
    set-option window mkdr_last_timestamp    ''
    set-option window mkdr_last_width        ''
    set-option window mkdr_last_config_hash  ''
}

# バッファクローズ時に CLOSE（daemon のバッファ状態を解放してメモリリーク防止）
# kak_opt_filetype で markdown バッファのみに限定（全バッファで mkdr send を起動しない）
hook global BufClose .* %sh{
    [ "$kak_opt_filetype" = "markdown" ] || exit 0
    mkdr send --close --session "$kak_session" --bufname "$kak_bufname" 2>/dev/null &
}

# セッション終了時に SHUTDOWN
hook global KakEnd .* %sh{
    mkdr send --shutdown --session "$kak_session" 2>/dev/null
}
```

### 検証ポイント 1

- Kakoune 上で `#` 置換、太字・イタリックのフェイス適用が確認できる
- cursor_context=0 の場合 NormalIdle がほぼ無負荷（~0ms）で動作する
- ウィンドウ幅変更時に thematic break が正しく更新される
- 複数ウィンドウで同一バッファを開いても独立して動作する

---

## Milestone 5 — デーモン Rust 側（UDS + 2スレッド + コアレスシング）

**目標**: `mkdr daemon` が UDS で待ち受け、コアレスシングしながらレンダリングする

### プロトコル（UDS SOCK_STREAM 上のテキストフレーム）

各 `mkdr send` 呼び出しが独立した接続を張るためメッセージは混ざらない。
content_len を省略し content は接続 EOF まで読む（io::copy でゼロコピーストリーム可能）。

```
PING     {session}\t{bufname}\t{timestamp}\t{cursor}\t{width}\t{config_hash_str}\t{client}\t{cmd_fifo}\n
RENDER   {session}\t{bufname}\t{timestamp}\t{cursor}\t{width}\t{client}\t{cmd_fifo}\t{config_len}\n
         {config_bytes}{content_bytes...接続EOF まで}
CLOSE    {session}\t{bufname}\n
SHUTDOWN {session}\n
```

- `config_hash_str`: `"{ts:016x}:{hash:016x}"` 形式の文字列。
  `ts` = 前回 RENDER 応答時のタイムスタンプ（fire-and-forget による上書き競合を防ぐ世代チェック用）。
  `hash` = FNV-1a（設定変更検出）。空文字列の場合 daemon は PING をスキップ（RENDER 待ち）。
- `cmd_fifo`: `kak_command_fifo` パス（将来の kak -p 排除に備えて今から渡す）
- `config_bytes`: `KEY=VALUE\n` 形式（mkdr send が kak_opt_* 環境変数から生成）
- `content_bytes`: バッファ内容（接続 EOF = コンテンツ終端）

### `src/daemon/protocol.rs`

```rust
// named struct を定義することで:
//   1. daemon/mod.rs の match で Message::Render(r) (tuple variant) を使える
//   2. handle_render(msg: RenderMsg, ...) のシグネチャが成立する
//   3. merge_message での msg.bufname() / msg.width() が各 struct の固有メソッドになる
//
// struct variant（Message::Render { ... }）と tuple variant（Message::Render(RenderMsg)）
// の混在は Rust ではコンパイルエラーになるため、必ず named struct + tuple variant で統一。

pub struct PingMsg {
    pub session: String, pub bufname: String, pub timestamp: u64,
    pub cursor: usize, pub width: usize,
    /// `"{ts:016x}:{hash:016x}"` 形式。kak_opt_mkdr_last_config_hash の生値をそのまま格納。
    /// 空文字列（初回 PING や GlobalSetOption リセット後）は handle_ping でスキップ。
    pub config_hash_str: String,
    pub client: String, pub cmd_fifo: String,
}

pub struct RenderMsg {
    pub session: String, pub bufname: String, pub timestamp: u64,
    pub cursor: usize, pub width: usize, pub client: String, pub cmd_fifo: String,
    pub config: Vec<u8>,
    // content は parse_message() 内で UTF-8 バリデーション済み String
    pub content: String,
}

pub struct CloseMsg { pub session: String, pub bufname: String }

pub enum Message {
    Ping(PingMsg),
    Render(RenderMsg),
    Close(CloseMsg),
    Shutdown(String),  // session のみ
}

impl Message {
    /// coalescing キー用。Close は width=0 を返す（実際の width は常に >0 のため衝突しない）。
    /// Shutdown は bufname="" を返す（実際の bufname は常に非空のため衝突しない）。
    pub fn bufname(&self) -> &str {
        match self {
            Message::Ping(m)     => &m.bufname,
            Message::Render(m)   => &m.bufname,
            Message::Close(m)    => &m.bufname,
            Message::Shutdown(_) => "",   // coalescing では使わない（別処理）
        }
    }
    pub fn width(&self) -> usize {
        match self {
            Message::Ping(m)   => m.width,
            Message::Render(m) => m.width,
            _                  => 0,   // Close/Shutdown は width=0（coalescing では使わない）
        }
    }
}

/// UDS stream から1メッセージを読む（接続ごとに呼ぶ）
pub fn parse_message(stream: impl BufRead) -> anyhow::Result<Message> {
    // ヘッダ行を1行読む
    // RENDER の場合:
    //   1. config_len バイトを読み config を取得
    //   2. 残り全てを読み込み String::from_utf8 で検証（Markdown は UTF-8 必須）
    //      → 失敗した場合は String::from_utf8_lossy で置換（ロバスト性優先）
    //   3. content は String として返す（Renderer::new(&str) に直接渡せる）
    // content_len は不要: 接続 EOF まで読む
    // EOF-on-first-read（check_alive 接続）は io::ErrorKind::UnexpectedEof として Err を返す。
    // daemon 側では eprintln せず silent ignore すること（次の技術的注意事項参照）。
}
```

### `src/daemon/response.rs`

```rust
/// daemon ロジックを kak -p 依存から切り離してテスト可能にする出力先抽象化。
/// handle_render / handle_ping に渡すことで、テスト時は RecordSink に差し替えられる。
/// kak.rs に置かず response.rs に置く理由: emit_to_kak の実装がここにあり同ファイルが自然。
/// width / config_hash は format_commands 経由で mkdr_last_width / mkdr_last_config_hash に保存。
pub trait EmitSink: Send {
    fn emit(&mut self, session: &str, client: &str, bufname: &str,
            timestamp: u64, width: usize,
            conceal: &[KakRange], faces: &[KakRange],
            config_hash: u64);
}

/// 本番用: emit_to_kak を呼び kak -p で送信する
pub struct KakPipeSink;
impl EmitSink for KakPipeSink {
    fn emit(&mut self, session: &str, client: &str, bufname: &str,
            timestamp: u64, width: usize,
            conceal: &[KakRange], faces: &[KakRange],
            config_hash: u64) {
        emit_to_kak(session, client, bufname, timestamp, width, conceal, faces, config_hash);
    }
}

/// kak -p でコマンドを送信する。fire-and-forget: render スレッドをブロックしない。
///
/// **設計**: `std::thread::spawn` で別スレッドを起動し、render スレッドはすぐに次の
/// メッセージ処理に移る。kak -p は 5-20ms かかるため、render スレッドをブロックすると
/// 連続タイプ時のコアレスシング効果が失われる（次のメッセージが channel に溜まる前に
/// render スレッドが kak -p を待ち続けるため）。
///
/// **タイムアウト**: spawn されたスレッドが `spawn_kak_p_with_timeout()` を呼び、
/// 2 秒のタイムアウト付きで kak -p を実行する。タイムアウト超過は eprintln のみ。
///
/// **注意**: RecordSink（テスト用）は emit 内で直接記録するため fire-and-forget にならない。
/// テストでは KakPipeSink（本番）の非同期挙動を前提にしないこと。
pub fn emit_to_kak(session: &str, client: &str, bufname: &str,
                   timestamp: u64, width: usize,
                   conceal: &[KakRange], faces: &[KakRange],
                   config_hash: u64)
{
    let cmd     = format_commands(client, bufname, timestamp, width, conceal, faces, config_hash);
    let session = session.to_string();
    // fire-and-forget: render スレッドは即座に次の処理へ移る
    std::thread::spawn(move || {
        spawn_kak_p_with_timeout(&session, &cmd);
    });
}

/// kak -p でコマンドを送信する（2 秒タイムアウト付き）。
/// `timeout 2 kak -p` を優先し、コマンドがなければ手動でスレッド+チャネルでタイムアウトを実装。
fn spawn_kak_p_with_timeout(session: &str, cmd: &str) {
    fn spawn_kak_p(session: &str, cmd: &str) -> std::io::Result<std::process::ExitStatus> {
        let mut child = std::process::Command::new("kak")
            .args(["-p", session])
            .stdin(std::process::Stdio::piped())
            .spawn()?;
        {
            let mut stdin = child.stdin.take().unwrap();
            use std::io::Write;
            stdin.write_all(cmd.as_bytes()).ok();
        }  // drop(stdin): EOF を送信してから wait
        child.wait()
    }

    // まず `timeout 2 kak -p` を試みる（Linux/GNU coreutils が存在する場合）
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

    // `timeout` コマンドが存在しない（ENOENT）場合のフォールバック
    if let Err(ref e) = result {
        if e.kind() == std::io::ErrorKind::NotFound {
            // kak -p を別スレッドで起動し 2 秒のタイムアウトを手動で実装
            let session = session.to_string();
            let cmd     = cmd.to_string();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(spawn_kak_p(&session, &cmd));
            });
            match rx.recv_timeout(std::time::Duration::from_secs(2)) {
                Ok(Ok(s)) if s.success() => return,
                _ => eprintln!("kak -p timed out or failed for session {session}"),
            }
            return;
        }
    }

    if !matches!(result, Ok(s) if s.success()) {
        eprintln!("kak -p failed or timed out for session {session}");
    }
}
```

### `src/daemon/state.rs`

```rust
pub struct BufState {
    pub last_rendered: u64,           // 最後に kak -p を呼んだタイムスタンプ
    pub last_config_hash: u64,        // FNV-1a hash（設定変更検出）
    pub last_cursor_context: usize,   // RENDER 時の cursor_context 保存値
    // キャッシュは必ず未フィルタ版。emit 直前に filter_cursor_overlap を適用。
    pub cached_conceal: Vec<KakRange>,
    pub cached_faces:   Vec<KakRange>,
}

// fnv1a() は paths.rs に一元定義し、state.rs / config.rs から pub(crate) use で参照する。
// paths.rs で session_hash() に使うため元々そこにあり、重複定義を避ける。
// use crate::paths::fnv1a;  ← daemon/state.rs でこれを使う

/// SessionState は (バッファ名, ウィンドウ幅) → BufState のマップ。
/// コアレスシングキー (bufname, width) と一致させることで、同一バッファを
/// 異なる幅のウィンドウで開いた場合もそれぞれ正しいキャッシュを参照できる。
/// ※ HashMap<String, BufState> だと幅違いの PING が別幅の BufState を参照してしまう。
pub struct SessionState {
    bufs: HashMap<(String, usize), BufState>,
}

impl SessionState {
    pub fn get_buf(&self, bufname: &str, width: usize) -> Option<&BufState> {
        self.bufs.get(&(bufname.to_string(), width))
    }
    pub fn get_buf_mut(&mut self, bufname: &str, width: usize) -> &mut BufState {
        self.bufs.entry((bufname.to_string(), width)).or_default()
    }
    /// CLOSE メッセージ受信時: そのバッファ名の全幅エントリを解放してメモリを回収する
    pub fn remove_buf(&mut self, bufname: &str) {
        self.bufs.retain(|(b, _), _| b != bufname);
    }
}
```

### `src/daemon/mod.rs`

```rust
pub fn run(session: &str) -> anyhow::Result<()> {
    // ソケットの親ディレクトリを 0700 で作成（初回起動時に必須）
    // ensure_session_dir() を呼ばないと bind() が ENOENT で失敗する
    ensure_session_dir(session)?;
    let sock_path = socket_path(session);
    // bind() 先行で試みる。先に remove_file するパターンは稼働中の daemon の
    // ソケットを破壊するリスクがある（複数 markdown バッファを同時に開いた場合等）。
    // EADDRINUSE なら既存ソケットが生きているか確認し、stale なら remove して再 bind。
    let listener = match UnixListener::bind(&sock_path) {
        Ok(l) => l,
        Err(e) if e.raw_os_error() == Some(libc::EADDRINUSE) => {
            // stale check: 接続できれば既存 daemon が稼働中 → 自分は終了
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

    // state と sink の所有権を render スレッドに move する（&mut 参照は 'static を満たせない）
    // 本番では KakPipeSink を渡す。テストでは RecordSink に差し替え可能。
    // run() は KakPipeSink で呼ぶ。テスト用に run_with_sink() を提供する。
    std::thread::spawn(move || render_loop(rx, state, KakPipeSink));

    // Accept スレッド（メインスレッド）: parse のみ担当し state に触れない
    for stream in listener.incoming() {
        let stream = stream?;
        // 接続ごとにメッセージをパース（RENDER は content を全読み込み）
        match parse_message(BufReader::new(stream)) {
            Ok(msg) => { tx.send(msg).ok(); }
            // UnexpectedEof = --check-alive 接続（ヘッダを書かずに即クローズ）: 正常
            Err(ref e) if matches!(e.downcast_ref::<io::Error>(), Some(e) if e.kind() == io::ErrorKind::UnexpectedEof) => {}
            Err(e) => eprintln!("parse error: {e}"),
        }
    }
    let _ = std::fs::remove_file(&sock_path);
    Ok(())
}

/// `sink` を受け取る設計により、テスト時に RecordSink を渡して emit 内容を検証できる
fn render_loop(rx: Receiver<Message>, mut state: SessionState, mut sink: impl EmitSink) {
    loop {
        // 最初の1件を待つ（ブロッキング）
        let first = match rx.recv() { Ok(m) => m, Err(_) => break };

        // Close/Shutdown はコアレスシングをスキップして即処理
        // （merge_message に渡すと bufname()/width() の番兵値が必要になり設計が複雑化するため）
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

        // バッファごとに最新1件だけ処理
        for (_, msg) in pending.drain() {
            match msg {
                Message::Render(r) => handle_render(r, &mut state, &mut sink),
                Message::Ping(p)   => handle_ping(p, &mut state, &mut sink),
                _                  => unreachable!("Close/Shutdown は上で処理済み"),
            }
        }
    }
}

/// PING と RENDER が競合する場合は RENDER を優先（同じ (bufname, width) キーで最新1件のみ保持）。
/// キーは `(String, usize)` タプル（bufname, width の組み合わせ）。
/// `format!("{}\x00{}", ...)` による String 確保を避けてタプルで直接比較する。
/// 同じバッファを異なる幅のウィンドウで開いている場合、幅が異なればそれぞれ独立してレンダリングする。
/// （thematic break / code fence border の文字数が幅依存のため）
///
/// Close / Shutdown は**コアレスシング対象外**。
/// render_loop 内で merge_message を通さず直接処理する（後述の render_loop 実装参照）。
/// 理由: Close の width() = 0 という番兵値で PING/RENDER と衝突しないよう設計するよりも、
/// Close/Shutdown を直接処理する方が明確。
fn merge_message(map: &mut HashMap<(String, usize), Message>, msg: Message) {
    // Close/Shutdown は呼ばれないが防衛的チェック
    debug_assert!(matches!(msg, Message::Ping(_) | Message::Render(_)));
    let key = (msg.bufname().to_string(), msg.width());
    match map.get(&key) {
        Some(Message::Render(_)) if matches!(msg, Message::Ping(_)) => {}  // RENDER 優先
        _ => { map.insert(key, msg); }
    }
}
// render_loop でも HashMap 型を更新する:
// let mut pending: HashMap<(String, usize), Message> = HashMap::new();

fn handle_render(msg: RenderMsg, state: &mut SessionState, sink: &mut dyn EmitSink) {
    let config = Config::from_bytes(&msg.config).unwrap_or_default();
    let config_hash = fnv1a(&msg.config);  // emit と BufState 保存の両方で使うため先に計算
    let (conceal, faces) = Renderer::new(&msg.content, &config, msg.width).render_unfiltered();

    // emit 直前にカーソルフィルタを適用（スコープを閉じて conceal/faces の借用を解放）
    // Cow::Borrowed(context=0) はゼロコピー。Cow::Owned(context>0) はフィルタ済み Vec。
    {
        let fc = filter_cursor_overlap(&conceal, msg.cursor, config.cursor_context);
        let ff = filter_cursor_overlap(&faces,   msg.cursor, config.cursor_context);
        sink.emit(&msg.session, &msg.client, &msg.bufname,
                  msg.timestamp, msg.width, fc.as_ref(), ff.as_ref(), config_hash);
    }  // fc/ff のライフタイムをここで終了 → conceal/faces を move 可能にする

    // キャッシュは未フィルタ版を保存（PING 再利用時に新カーソル位置でフィルタを掛け直す）
    // width キーで BufState を取得（コアレスシングキーと一致）
    let buf = state.get_buf_mut(&msg.bufname, msg.width);
    buf.last_rendered        = msg.timestamp;
    buf.last_config_hash     = config_hash;
    buf.last_cursor_context  = config.cursor_context;  // PING 再利用時に参照
    buf.cached_conceal       = conceal;   // 未フィルタ版を保存
    buf.cached_faces         = faces;
}

fn handle_ping(msg: PingMsg, state: &mut SessionState, sink: &mut dyn EmitSink) {
    // RENDER が来る前に PING が届いた場合、BufState は未作成 → スキップ
    // （バッファを開いた直後や daemon 再起動直後に発生しうる。正常系として無視する）
    // width キーで BufState を取得（コアレスシングキーと一致。異なる幅の window を混同しない）
    let buf = match state.get_buf(&msg.bufname, msg.width) {
        Some(b) => b,
        None    => return,
    };

    // 世代 + 設定変更チェック: config_hash_str を "{ts:016x}:{hash:016x}" 形式でパースし
    // ts が last_rendered と一致し hash が last_config_hash と一致する場合のみキャッシュ再利用。
    // ts を含めることで fire-and-forget の古い kak -p が上書きしても世代不一致でスキップできる。
    // 空文字列（初回 PING / GlobalSetOption リセット後）は即スキップ（RENDER 待ち）。
    let Some((cached_ts, cached_hash)) = parse_config_hash_str(&msg.config_hash_str) else { return; };
    if buf.last_rendered    != cached_ts   { return; }  // 世代不一致
    if buf.last_config_hash != cached_hash { return; }  // 設定変更

    // cursor_context は Config::default() ではなく RENDER 時に保存した値を使用
    // （Config::default() を使うと cursor_context 変更直後の PING で誤フィルタが掛かる）
    let cursor_context  = buf.last_cursor_context;
    let config_hash     = buf.last_config_hash;   // PING emit にも config_hash を含める

    // 未フィルタキャッシュに新しいカーソル位置でフィルタを掛けて emit
    // Cow::Borrowed はゼロコピー（context=0 のデフォルトケースで Vec 確保なし）
    let fc = filter_cursor_overlap(&buf.cached_conceal, msg.cursor, cursor_context);
    let ff = filter_cursor_overlap(&buf.cached_faces,   msg.cursor, cursor_context);

    sink.emit(&msg.session, &msg.client, &msg.bufname,
              msg.timestamp, msg.width, fc.as_ref(), ff.as_ref(), config_hash);
}

/// `"{ts:016x}:{hash:016x}"` 形式から (ts, hash) を取り出す。
/// 形式不正・空文字列の場合は None を返す（RENDER 待ち → スキップ）。
fn parse_config_hash_str(s: &str) -> Option<(u64, u64)> {
    let (ts_hex, hash_hex) = s.split_once(':')?;
    let ts   = u64::from_str_radix(ts_hex,   16).ok()?;
    let hash = u64::from_str_radix(hash_hex, 16).ok()?;
    Some((ts, hash))
}
```

### `parse_message` 単体テスト

テキストファイル形式（`tests/parse_message/`）と Rust インラインを組み合わせる:

```rust
// tests/parse_message_test.rs

fn parse(input: &[u8]) -> anyhow::Result<Message> {
    parse_message(std::io::BufReader::new(input))
}

// ケース1: PING 正常系（全フィールド）
#[test]
fn ping_basic() {
    let input = b"PING\tsession1\tbuf.md\t42\t10\t80\tdeadbeef00000000\tclient1\t/tmp/fifo\n";
    let msg = parse(input).unwrap();
    assert!(matches!(msg, Message::Ping { timestamp: 42, cursor: 10, width: 80, .. }));
}

// ケース2: RENDER 正常系
#[test]
fn render_basic() {
    let config = b"kak_opt_mkdr_cursor_context=0\n";
    let content = b"# Hello\n";
    let header = format!("RENDER\tsession1\tbuf.md\t42\t1\t80\tclient1\t/tmp/fifo\t{}\n",
                         config.len());
    let mut input = header.into_bytes();
    input.extend_from_slice(config);
    input.extend_from_slice(content);
    let msg = parse(&input).unwrap();
    assert!(matches!(msg, Message::Render { timestamp: 42, .. }));
}

// ケース3: CLOSE
#[test]
fn close_basic() {
    let msg = parse(b"CLOSE\tsession1\tbuf.md\n").unwrap();
    assert!(matches!(msg, Message::Close { .. }));
}

// ケース4: SHUTDOWN
#[test]
fn shutdown_basic() {
    let msg = parse(b"SHUTDOWN\tsession1\n").unwrap();
    assert!(matches!(msg, Message::Shutdown { .. }));
}

// ケース5: RENDER で config_len=0（設定バイト列なし）
#[test]
fn render_no_config() {
    let content = b"plain text\n";
    let header = format!("RENDER\ts\tb\t1\t1\t80\tc\t/f\t0\n");
    let mut input = header.into_bytes();
    input.extend_from_slice(content);
    let msg = parse(&input).unwrap();
    // config_len=0 → Config::default() が使われる
    assert!(matches!(msg, Message::Render { .. }));
}

// ケース6: 不正なヘッダ（フィールド不足）→ Err を返す
#[test]
fn bad_header() {
    let result = parse(b"PING\tsession_only\n");
    assert!(result.is_err());
}

// ケース7: RENDER で content が invalid UTF-8 → lossy 変換で Ok を返す
#[test]
fn render_invalid_utf8_content() {
    let config = b"";
    let header = format!("RENDER\ts\tb\t1\t1\t80\tc\t/f\t0\n");
    let mut input = header.into_bytes();
    input.extend_from_slice(config);
    input.extend_from_slice(b"\xff\xfe invalid utf8 \xed\xa0\x80");
    let msg = parse(&input).unwrap();
    // 無効 UTF-8 は from_utf8_lossy で U+FFFD に変換して Ok
    assert!(matches!(msg, Message::Render { .. }));
}
```

### daemon 統合テスト

**テスト隔離戦略**: 各テストが一意のセッション名を使い、`DaemonHandle` が `Drop` で確実にクリーンアップする。
並列実行（`cargo test`）でも EADDRINUSE が発生しない。

```rust
// tests/common.rs

pub fn unique_session() -> String {
    // プロセス ID + サブナノ秒でテスト間の衝突を防ぐ
    format!("mkdrtest-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos())
}

/// テスト終了時に daemon を確実に停止しソケットを削除する RAII ガード
pub struct DaemonHandle {
    session: String,
    _child:  std::process::Child,
}

impl DaemonHandle {
    pub fn spawn(session: &str) -> Self {
        let child = std::process::Command::new(env!("CARGO_BIN_EXE_mkdr"))
            .args(["daemon", "--session", session])
            .spawn()
            .expect("daemon spawn failed");
        // ソケット作成を待つ（最大 1 秒）
        for _ in 0..20 {
            if std::process::Command::new(env!("CARGO_BIN_EXE_mkdr"))
                .args(["send", "--check-alive", "--session", session])
                .status().map(|s| s.success()).unwrap_or(false) { break; }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        DaemonHandle { session: session.to_string(), _child: child }
    }
}

impl Drop for DaemonHandle {
    fn drop(&mut self) {
        // SHUTDOWN 送信（失敗しても続行）
        let _ = std::process::Command::new(env!("CARGO_BIN_EXE_mkdr"))
            .args(["send", "--shutdown", "--session", &self.session])
            .status();
        // ソケットファイル残留を確実に削除
        let sock = socket_path(&self.session);
        let _ = std::fs::remove_file(&sock);
    }
}
```

統合テストシナリオ:

```
tests/integration/
├── render_basic.rs      # daemon 起動 → RENDER 送信 → RecordSink の emit 内容を検証
├── coalescing.rs        # 同一バッファに N 件 RENDER → emit が最後の1件のみ
├── ping_cache.rs        # RENDER 後に PING → キャッシュ再利用（emit が同じ内容）
├── ping_skip_stale.rs   # RENDER 完了前に PING → スキップ（emit されない）
├── ping_config_hash.rs  # config 変更後 PING → config_hash 不一致でスキップ
├── close_cleanup.rs     # CLOSE 後に PING → BufState が削除されスキップ
└── shutdown.rs          # SHUTDOWN → daemon 正常終了
```

**注意**: 統合テストは `RecordSink` を使って emit 内容を検証する。
`kak -p` は呼ばれないため kak セッションなしでテスト可能。
`render_loop` に `RecordSink` を渡すには `daemon::run_with_sink(session, sink)` 形式の
テスト用エントリポイントを追加する（本番の `run()` は `KakPipeSink` で呼ぶ）。

