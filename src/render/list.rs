use crate::kak::{KakRange, escape_markup};
use crate::offset::byte_to_line_col;
use super::RenderCtx;

pub fn render(
    range:      std::ops::Range<usize>,
    depth:      usize,      // 0-indexed ネスト深さ
    is_ordered: bool,
    _start_num: u64,
    ctx:        &RenderCtx<'_>,
    conceal:    &mut Vec<KakRange>,
    faces:      &mut Vec<KakRange>,
) {
    let src = &ctx.content[range.clone()];

    // アイテム先頭のマーカーをソースから実測
    // 先頭にインデント（スペース/タブ）がある場合も考慮
    let trimmed_start = src.len() - src.trim_start_matches(|c: char| c == ' ' || c == '\t').len();
    let abs_start = range.start + trimmed_start;

    let (line_n, col_n) = byte_to_line_col(ctx.starts, abs_start);

    if is_ordered {
        // 順序付きリスト: `1. ` の `1.` 部分にフェイスを適用
        // 数字 + ピリオドの長さを実測
        let marker_src = &src[trimmed_start..];
        let dot_pos = marker_src.find('.').unwrap_or(0);
        // `1.` は dot_pos+1 バイト（ピリオドまで含む）
        let marker_bytes = dot_pos + 1;

        faces.push(KakRange {
            line_start: line_n,
            col_start:  col_n,
            line_end:   line_n,
            col_end:    col_n + marker_bytes - 1,
            text:       "MkdrOrderedList".to_string(),
        });
    } else {
        // 順不同リスト: `-`/`*`/`+` を bullet_chars[depth % 3] に置換
        let ch = ctx.config.bullet_chars[depth % 3];
        let replacement = escape_markup(&ch.to_string());

        conceal.push(KakRange {
            line_start: line_n,
            col_start:  col_n,
            line_end:   line_n,
            col_end:    col_n,   // マーカー1バイト（ASCII）
            text:       replacement,
        });
    }
}
