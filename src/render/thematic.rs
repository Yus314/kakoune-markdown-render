use crate::kak::{KakRange, escape_markup};
use crate::offset::byte_to_line_col;
use super::RenderCtx;

pub fn render(
    range:   std::ops::Range<usize>,
    ctx:     &RenderCtx<'_>,
    conceal: &mut Vec<KakRange>,
    faces:   &mut Vec<KakRange>,
) {
    // Event::Rule の range は `---`/`***`/`___` を含む行全体（改行手前まで）
    let (line_s, col_s) = byte_to_line_col(ctx.starts, range.start);
    let (line_e, col_e) = byte_to_line_col(ctx.starts, range.end.saturating_sub(1));

    // window_width 分のテーマ区切り文字で置換（conceal）
    // {MkdrThematicBreak} マークアップで明示的な色を適用
    let ch = ctx.config.thematic_char;
    let border_chars: String = std::iter::repeat(escape_markup(&ch.to_string()))
        .take(ctx.window_width)
        .collect();
    let replacement = format!("{{MkdrThematicBreak}}{border_chars}");

    conceal.push(KakRange {
        line_start: line_s,
        col_start:  col_s,
        line_end:   line_e,
        col_end:    col_e,
        text:       replacement,
    });

    // フェイス適用
    faces.push(KakRange {
        line_start: line_s,
        col_start:  col_s,
        line_end:   line_e,
        col_end:    col_e,
        text:       "MkdrThematicBreak".to_string(),
    });
}
