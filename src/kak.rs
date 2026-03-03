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

/// 文字の端末表示幅を推定（PUA 文字は Nerd Font 前提で 2 セル）。
pub fn char_display_width(c: char) -> usize {
    let cp = c as u32;
    match cp {
        // ASCII printable
        0x20..=0x7E => 1,
        // Hangul Jamo
        0x1100..=0x115F | 0x2329..=0x232A => 2,
        // CJK misc, ideographs, compatibility
        0x2E80..=0x303E | 0x3041..=0x33BF |
        0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xA000..=0xA4CF |
        0xAC00..=0xD7AF |
        0xF900..=0xFAFF | 0xFE10..=0xFE19 | 0xFE30..=0xFE6F |
        0xFF01..=0xFF60 | 0xFFE0..=0xFFE6 |
        0x1F000..=0x1FFFD |
        0x20000..=0x2FFFD | 0x30000..=0x3FFFD => 2,
        // Private Use Areas (Nerd Font icons are typically 2 cells wide)
        0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD => 2,
        _ => 1,
    }
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
         set-option window mkdr_conceal {ts}{conceal_specs}\n\
         set-option window mkdr_faces   {ts}{faces_specs}\n\
         set-option window mkdr_last_timestamp {ts}\n\
         set-option window mkdr_last_width {width}\n\
         set-option window mkdr_last_config_hash {hash_str}\n\
         }}\n\
         }} catch %{{}}",
        client = kakquote(client),
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

    #[test]
    fn char_display_width_ascii() {
        assert_eq!(char_display_width('A'), 1);
        assert_eq!(char_display_width('#'), 1);
        assert_eq!(char_display_width(' '), 1);
    }

    #[test]
    fn char_display_width_box_drawing() {
        assert_eq!(char_display_width('─'), 1);
        assert_eq!(char_display_width('▌'), 1);
        assert_eq!(char_display_width('•'), 1);
    }

    #[test]
    fn char_display_width_cjk() {
        assert_eq!(char_display_width('日'), 2);
        assert_eq!(char_display_width('漢'), 2);
    }

    #[test]
    fn char_display_width_nerd_font_pua() {
        // Nerd Font nf-md-numeric_N_circle_outline (Supplementary PUA-A)
        assert_eq!(char_display_width('\u{F0CA1}'), 2);
        assert_eq!(char_display_width('\u{F0CA3}'), 2);
        assert_eq!(char_display_width('\u{F0CAB}'), 2);
    }

    #[test]
    fn char_display_width_bmp_pua() {
        // BMP PUA range (Nerd Font icons)
        assert_eq!(char_display_width('\u{E000}'), 2);
        assert_eq!(char_display_width('\u{F000}'), 2);
    }
}
