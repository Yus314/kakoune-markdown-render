use crate::kak::{KakRange, escape_markup};
use crate::offset::byte_to_line_col;
use super::RenderCtx;

pub fn render(
    range:   std::ops::Range<usize>,
    ctx:     &RenderCtx<'_>,
    conceal: &mut Vec<KakRange>,
    _faces:  &mut Vec<KakRange>,
) {
    // Start(Tag::BlockQuote) の range はブロック引用全体
    // 各行の先頭の `>` マーカーをソースから実測して置換する
    let src = &ctx.content[range.clone()];

    let mut offset = range.start;
    for line in src.split('\n') {
        // 行の先頭から `>` を探す（インデントがある場合も考慮）
        let trimmed = line.trim_start_matches(' ').trim_start_matches('\t');
        if let Some(marker_pos) = trimmed.find('>') {
            // ソース内での `>` の位置（スペース/タブ分を考慮）
            let indent_len = line.len() - trimmed.len();
            let abs_marker = offset + indent_len + marker_pos;

            let (line_n, col_n) = byte_to_line_col(ctx.starts, abs_marker);

            // `> ` または `>` だけを置換
            let ch = ctx.config.blockquote_char;
            let replacement = format!("{} ", escape_markup(&ch.to_string()));

            // `>` の次がスペースなら2バイト、そうでなければ1バイト置換
            let marker_len = if trimmed.as_bytes().get(marker_pos + 1) == Some(&b' ') {
                2
            } else {
                1
            };

            conceal.push(KakRange {
                line_start: line_n,
                col_start:  col_n,
                line_end:   line_n,
                col_end:    col_n + marker_len - 1,
                text:       replacement,
            });
        }
        // 次の行へ（\n の分 +1）
        offset += line.len() + 1;
        if offset > range.end { break; }
    }
}
