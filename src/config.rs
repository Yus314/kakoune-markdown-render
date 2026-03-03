use crate::paths::fnv1a;

#[derive(Debug, Clone, PartialEq)]
pub enum Preset {
    Default,
    Minimal,
    Ascii,
}

impl Default for Preset {
    fn default() -> Self { Preset::Default }
}

impl Preset {
    fn from_str(s: &str) -> Self {
        match s {
            "minimal" => Preset::Minimal,
            "ascii"   => Preset::Ascii,
            _         => Preset::Default,
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Preset::Default => "default",
            Preset::Minimal => "minimal",
            Preset::Ascii   => "ascii",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub cursor_context: usize,
    /// H1〜H6 のプレフィックス文字。0-indexed（H1 = heading_char[0]）。
    pub heading_char: [char; 6],
    pub heading_setext: bool,
    pub thematic_char: char,
    pub blockquote_char: char,
    pub bullet_chars: [char; 3],
    pub task_unchecked: char,
    pub task_checked:   char,
    pub code_fence_char: char,
    pub enable_bold:      bool,
    pub enable_italic:    bool,
    pub enable_code_span: bool,
    pub enable_link:      bool,
    pub enable_table:     bool,
    pub preset: Preset,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            cursor_context:  1,
            heading_char:    ['\u{F0CA1}', '\u{F0CA3}', '\u{F0CA5}', '\u{F0CA7}', '\u{F0CA9}', '\u{F0CAB}'],
            heading_setext:  false,
            thematic_char:   '─',
            blockquote_char: '▎',
            bullet_chars:    ['•', '◦', '▸'],
            task_unchecked:  '☐',
            task_checked:    '☑',
            code_fence_char: '▔',
            enable_bold:      true,
            enable_italic:    true,
            enable_code_span: true,
            enable_link:      true,
            enable_table:     true,
            preset: Preset::Default,
        }
    }
}

impl Config {
    /// kak_opt_mkdr_* 環境変数から設定を読み込む（mkdr send 側で使用）
    pub fn from_env() -> Self {
        Self::from_env_inner(|key| std::env::var(key).ok().filter(|v| !v.is_empty()))
    }

    fn from_env_inner(lookup: impl Fn(&str) -> Option<String>) -> Self {
        let get_char = |key: &str, default: char| -> char {
            lookup(key)
                .and_then(|s| s.chars().next())
                .unwrap_or(default)
        };
        let get_bool = |key: &str, default: bool| -> bool {
            lookup(key)
                .map(|s| s == "true")
                .unwrap_or(default)
        };
        let get_usize = |key: &str, default: usize| -> usize {
            lookup(key)
                .and_then(|s| s.parse().ok())
                .unwrap_or(default)
        };

        let def = Config::default();
        Config {
            cursor_context:  get_usize("kak_opt_mkdr_cursor_context", def.cursor_context),
            heading_char: [
                get_char("kak_opt_mkdr_heading_char_1", def.heading_char[0]),
                get_char("kak_opt_mkdr_heading_char_2", def.heading_char[1]),
                get_char("kak_opt_mkdr_heading_char_3", def.heading_char[2]),
                get_char("kak_opt_mkdr_heading_char_4", def.heading_char[3]),
                get_char("kak_opt_mkdr_heading_char_5", def.heading_char[4]),
                get_char("kak_opt_mkdr_heading_char_6", def.heading_char[5]),
            ],
            heading_setext:  get_bool("kak_opt_mkdr_heading_setext", def.heading_setext),
            thematic_char:   get_char("kak_opt_mkdr_thematic_char",   def.thematic_char),
            blockquote_char: get_char("kak_opt_mkdr_blockquote_char", def.blockquote_char),
            bullet_chars: [
                get_char("kak_opt_mkdr_bullet_char_1", def.bullet_chars[0]),
                get_char("kak_opt_mkdr_bullet_char_2", def.bullet_chars[1]),
                get_char("kak_opt_mkdr_bullet_char_3", def.bullet_chars[2]),
            ],
            task_unchecked:  get_char("kak_opt_mkdr_task_unchecked",  def.task_unchecked),
            task_checked:    get_char("kak_opt_mkdr_task_checked",    def.task_checked),
            code_fence_char: get_char("kak_opt_mkdr_code_fence_char", def.code_fence_char),
            enable_bold:      get_bool("kak_opt_mkdr_enable_bold",      def.enable_bold),
            enable_italic:    get_bool("kak_opt_mkdr_enable_italic",    def.enable_italic),
            enable_code_span: get_bool("kak_opt_mkdr_enable_code_span", def.enable_code_span),
            enable_link:      get_bool("kak_opt_mkdr_enable_link",      def.enable_link),
            enable_table:     get_bool("kak_opt_mkdr_enable_table",     def.enable_table),
            preset: lookup("kak_opt_mkdr_preset")
                .map(|s| Preset::from_str(&s))
                .unwrap_or_default(),
        }
    }

    /// テスト用: key-value ペアから Config を構築（環境変数に依存しない）
    #[cfg(test)]
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        Self::from_env_inner(|key| {
            pairs.iter()
                .find(|(k, _)| *k == key)
                .filter(|(_, v)| !v.is_empty())
                .map(|(_, v)| v.to_string())
        })
    }

    /// KEY=VALUE\n 形式のバイト列からパース（daemon 受信時）
    pub fn from_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        let text = std::str::from_utf8(bytes)?;
        let pairs: Vec<(&str, &str)> = text
            .lines()
            .filter_map(|line| line.split_once('='))
            .collect();
        Ok(Self::from_env_inner(|key| {
            pairs.iter()
                .find(|(k, _)| *k == key)
                .filter(|(_, v)| !v.is_empty())
                .map(|(_, v)| v.to_string())
        }))
    }

    /// KEY=VALUE\n 形式にシリアライズ（mkdr send 送信時）
    /// フィールドは必ずアルファベット順で出力する（hash の安定性のため）。
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = String::new();
        // アルファベット順
        out.push_str(&format!("kak_opt_mkdr_blockquote_char={}\n",  self.blockquote_char));
        out.push_str(&format!("kak_opt_mkdr_bullet_char_1={}\n",    self.bullet_chars[0]));
        out.push_str(&format!("kak_opt_mkdr_bullet_char_2={}\n",    self.bullet_chars[1]));
        out.push_str(&format!("kak_opt_mkdr_bullet_char_3={}\n",    self.bullet_chars[2]));
        out.push_str(&format!("kak_opt_mkdr_code_fence_char={}\n",  self.code_fence_char));
        out.push_str(&format!("kak_opt_mkdr_cursor_context={}\n",   self.cursor_context));
        out.push_str(&format!("kak_opt_mkdr_enable_bold={}\n",      self.enable_bold));
        out.push_str(&format!("kak_opt_mkdr_enable_code_span={}\n", self.enable_code_span));
        out.push_str(&format!("kak_opt_mkdr_enable_italic={}\n",    self.enable_italic));
        out.push_str(&format!("kak_opt_mkdr_enable_link={}\n",      self.enable_link));
        out.push_str(&format!("kak_opt_mkdr_enable_table={}\n",     self.enable_table));
        out.push_str(&format!("kak_opt_mkdr_heading_char_1={}\n",   self.heading_char[0]));
        out.push_str(&format!("kak_opt_mkdr_heading_char_2={}\n",   self.heading_char[1]));
        out.push_str(&format!("kak_opt_mkdr_heading_char_3={}\n",   self.heading_char[2]));
        out.push_str(&format!("kak_opt_mkdr_heading_char_4={}\n",   self.heading_char[3]));
        out.push_str(&format!("kak_opt_mkdr_heading_char_5={}\n",   self.heading_char[4]));
        out.push_str(&format!("kak_opt_mkdr_heading_char_6={}\n",   self.heading_char[5]));
        out.push_str(&format!("kak_opt_mkdr_heading_setext={}\n",   self.heading_setext));
        out.push_str(&format!("kak_opt_mkdr_preset={}\n",           self.preset.as_str()));
        out.push_str(&format!("kak_opt_mkdr_task_checked={}\n",     self.task_checked));
        out.push_str(&format!("kak_opt_mkdr_task_unchecked={}\n",   self.task_unchecked));
        out.push_str(&format!("kak_opt_mkdr_thematic_char={}\n",    self.thematic_char));
        out.into_bytes()
    }

    /// FNV-1a ハッシュ（PING の config_hash フィールド用）
    #[cfg(test)]
    pub fn hash(&self) -> u64 {
        fnv1a(&self.to_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_hash_stable() {
        let c = Config::default();
        assert_eq!(c.hash(), c.hash());
    }

    #[test]
    fn config_hash_changes_on_field() {
        let c1 = Config::default();
        let c2 = Config { thematic_char: '=', ..Config::default() };
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
        assert_eq!(keys, sorted, "to_bytes() field order must be alphabetical");
    }

    #[test]
    fn from_bytes_roundtrip() {
        let orig = Config::default();
        let bytes = orig.to_bytes();
        let parsed = Config::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.thematic_char, orig.thematic_char);
        assert_eq!(parsed.cursor_context, orig.cursor_context);
    }

    #[test]
    fn from_pairs_override() {
        let c = Config::from_pairs(&[
            ("kak_opt_mkdr_thematic_char", "="),
            ("kak_opt_mkdr_cursor_context", "2"),
        ]);
        assert_eq!(c.thematic_char, '=');
        assert_eq!(c.cursor_context, 2);
    }
}
