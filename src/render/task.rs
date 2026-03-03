use crate::kak::{KakRange, escape_markup};
use crate::offset::byte_to_line_col;
use super::RenderCtx;

pub fn render(
    range:   std::ops::Range<usize>,
    checked: bool,
    ctx:     &RenderCtx<'_>,
    conceal: &mut Vec<KakRange>,
    faces:   &mut Vec<KakRange>,
) {
    // pulldown-cmark の range は `[ ]` または `[x]` の3バイトを正確に指す
    let (line_s, col_s) = byte_to_line_col(ctx.starts, range.start);
    let (line_e, col_e) = byte_to_line_col(ctx.starts, range.end.saturating_sub(1));

    let (ch, face) = if checked {
        (ctx.config.task_checked,   "MkdrTaskChecked")
    } else {
        (ctx.config.task_unchecked, "MkdrTaskUnchecked")
    };

    // `[ ]` / `[x]` (3バイト) を1文字に置換（conceal）
    conceal.push(KakRange {
        line_start: line_s,
        col_start:  col_s,
        line_end:   line_e,
        col_end:    col_e,
        text:       escape_markup(&ch.to_string()),
    });

    // フェイス適用
    faces.push(KakRange {
        line_start: line_s,
        col_start:  col_s,
        line_end:   line_e,
        col_end:    col_e,
        text:       face.to_string(),
    });
}
