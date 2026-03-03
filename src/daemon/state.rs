use std::collections::HashMap;
use crate::kak::KakRange;

pub struct BufState {
    pub last_rendered:       u64,
    pub last_config_hash:    u64,
    pub last_cursor_context: usize,
    // キャッシュは必ず未フィルタ版。emit 直前に filter_cursor_overlap を適用。
    pub cached_conceal: Vec<KakRange>,
    pub cached_faces:   Vec<KakRange>,
}

impl Default for BufState {
    fn default() -> Self {
        BufState {
            last_rendered:       0,
            last_config_hash:    0,
            last_cursor_context: 0,
            cached_conceal:      Vec::new(),
            cached_faces:        Vec::new(),
        }
    }
}

/// (バッファ名, ウィンドウ幅) → BufState のマップ。
/// コアレスシングキー (bufname, width) と一致させ、同一バッファを異なる幅の
/// ウィンドウで開いた場合もそれぞれ正しいキャッシュを参照できる。
#[derive(Default)]
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

    /// CLOSE メッセージ受信時: そのバッファ名の全幅エントリを解放
    pub fn remove_buf(&mut self, bufname: &str) {
        self.bufs.retain(|(b, _), _| b != bufname);
    }
}
