#[derive(Clone)]
pub struct KakRange {
    pub line_start: usize,
    pub col_start:  usize,
    pub line_end:   usize,  // inclusive（Kakoune 仕様）
    pub col_end:    usize,
    /// `replace-ranges`（conceal）用: markup string（例: `{MkdrBold}▌{/}`）
    /// `ranges`（faces）用: フェイス名（例: `MkdrBold`）
    pub text: String,
}

impl KakRange {
    /// Kakoune range-specs 形式に変換: `line.col,line.col|text`
    pub fn to_spec(&self) -> String {
        format!(
            "{}.{},{}.{}|{}",
            self.line_start, self.col_start,
            self.line_end,   self.col_end,
            self.text
        )
    }
}

pub fn kakquote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

pub fn escape_markup(s: &str) -> String {
    // replace-ranges の markup string では \ | { を全てエスケープ必須。
    // \ は他の置換のプレフィックスになるので最初に処理する。
    s.replace('\\', "\\\\").replace('|', "\\|").replace('{', "\\{")
}

/// conceal + faces のコマンドを生成。
/// try-client / try-catch でクライアント消滅・バッファ消滅を安全に無視する。
/// timestamp は Kakoune 側で自動検証される。
/// `mkdr_last_timestamp` / `mkdr_last_width` はここで設定する（非オプティミスティック）。
/// `mkdr_last_config_hash` は `"{ts:016x}:{hash:016x}"` 形式で保存する。
pub fn format_commands(
    client:      &str,
    bufname:     &str,
    timestamp:   u64,
    width:       usize,
    conceal:     &[KakRange],
    faces:       &[KakRange],
    config_hash: u64,
) -> String {
    let conceal_specs: String = conceal
        .iter()
        .map(|r| format!(" {}", kakquote(&r.to_spec())))
        .collect();
    let faces_specs: String = faces
        .iter()
        .map(|r| format!(" {}", kakquote(&r.to_spec())))
        .collect();

    let config_hash_str = format!("{:016x}:{:016x}", timestamp, config_hash);

    format!(
        "try %{{\n\
         evaluate-commands -try-client {client} %{{\n\
         evaluate-commands %sh{{\n\
           [ \"$kak_bufname\" = {bufname} ] || echo 'fail buffer-mismatch'\n\
         }}\n\
         set-option window mkdr_conceal {ts}{conceal_specs}\n\
         set-option window mkdr_faces   {ts}{faces_specs}\n\
         set-option window mkdr_last_timestamp {ts}\n\
         set-option window mkdr_last_width {width}\n\
         set-option window mkdr_last_config_hash {hash_str}\n\
         }}\n\
         }} catch %{{}}",
        client = kakquote(client),
        bufname = kakquote(bufname),
        ts = timestamp,
        width = width,
        conceal_specs = conceal_specs,
        faces_specs = faces_specs,
        hash_str = kakquote(&config_hash_str),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_backslash_first() {
        assert_eq!(escape_markup("a\\|b"), "a\\\\\\|b");
    }

    #[test]
    fn escape_pipe() {
        assert_eq!(escape_markup("a|b"), "a\\|b");
    }

    #[test]
    fn escape_brace() {
        assert_eq!(escape_markup("a{b"), "a\\{b");
    }

    #[test]
    fn escape_empty() {
        assert_eq!(escape_markup(""), "");
    }

    #[test]
    fn escape_japanese() {
        assert_eq!(escape_markup("日本語"), "日本語");
    }

    #[test]
    fn format_commands_empty() {
        let s = format_commands("client1", "buf1", 42, 80, &[], &[], 0xdeadbeef);
        assert!(s.contains("set-option window mkdr_conceal 42"));
        assert!(s.contains("set-option window mkdr_faces   42"));
        assert!(s.contains("set-option window mkdr_last_timestamp 42"));
        assert!(s.contains("set-option window mkdr_last_width 80"));
        assert!(s.contains("set-option window mkdr_last_config_hash"));
        assert!(s.contains("evaluate-commands -try-client 'client1'"));
        assert!(s.contains("$kak_bufname\" = 'buf1'"));
    }

    #[test]
    fn format_commands_try_catch_wrapping() {
        let s = format_commands("c", "b", 1, 80, &[], &[], 0);
        assert!(s.starts_with("try %{"));
        assert!(s.ends_with("} catch %{}"));
    }

    #[test]
    fn format_commands_config_hash_format() {
        let s = format_commands("c", "b", 42, 80, &[], &[], 0xdeadbeef12345678);
        assert!(s.contains("mkdr_last_config_hash '000000000000002a:deadbeef12345678'"));
    }
}
