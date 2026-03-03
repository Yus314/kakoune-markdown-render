use crate::kak::{KakRange, escape_markup};
use crate::offset::byte_to_line_col;
use super::RenderCtx;

pub fn render(
    range:   std::ops::Range<usize>,
    level:   usize,
    ctx:     &RenderCtx<'_>,
    conceal: &mut Vec<KakRange>,
    faces:   &mut Vec<KakRange>,
) {
    let src = &ctx.content[range.clone()];

    // `#+ ` プレフィックスをバイト数で実測
    let prefix_bytes = {
        let mut i = 0;
        for b in src.bytes() {
            if b == b'#' { i += 1; } else { break; }
        }
        // ATX 見出しの必須スペース（`# title`）
        if src.as_bytes().get(i) == Some(&b' ') { i += 1; }
        i
    };

    if prefix_bytes == 0 { return; }

    let (line_s, col_s) = byte_to_line_col(ctx.starts, range.start);
    let (line_e, col_e) = byte_to_line_col(ctx.starts, range.end.saturating_sub(1));

    // heading_char は 0-indexed (H1=0, H6=5)
    let idx = (level - 1).min(5);
    let ch = ctx.config.heading_char[idx];

    // `# ` (prefix_bytes バイト) をアイコン文字に置換（conceal）
    // 右端の # の位置にアイコンを配置し、左側を空白でインデント
    // 例: `### ` (4セル) → `  󰲥 ` (2空白 + icon + 空白 = 4セル)
    let col_prefix_e = col_s + prefix_bytes - 1;
    let indent = " ".repeat(level.saturating_sub(1));
    let replacement = format!("{}{} ", indent, escape_markup(&ch.to_string()));

    conceal.push(KakRange {
        line_start: line_s,
        col_start:  col_s,
        line_end:   line_s,
        col_end:    col_prefix_e,
        text:       replacement,
    });

    // 行全体に見出しフェイス適用
    faces.push(KakRange {
        line_start: line_s,
        col_start:  col_s,
        line_end:   line_e,
        col_end:    col_e,
        text:       format!("MkdrHeading{}", level),
    });
}
