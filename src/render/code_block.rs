use crate::kak::{KakRange, escape_markup};
use crate::offset::byte_to_line_col;
use super::RenderCtx;

pub fn render(
    range:   std::ops::Range<usize>,
    ctx:     &RenderCtx<'_>,
    conceal: &mut Vec<KakRange>,
    faces:   &mut Vec<KakRange>,
) {
    let src = &ctx.content[range.clone()];

    // フェンス文字（`` ` `` または `~`）と長さをソースから実測
    let fence_char = src.bytes().next().unwrap_or(b'`');
    let fence_len = src.bytes().take_while(|&b| b == fence_char).count();
    if fence_len < 3 { return; }

    // 開始フェンス行（最初の行）
    let open_line_end = src.find('\n').unwrap_or(src.len());
    let open_src = &src[..open_line_end];

    // 言語ラベル（フェンス文字の後）
    let lang = open_src[fence_len..].trim();

    let (open_line, open_col) = byte_to_line_col(ctx.starts, range.start);

    // 開始フェンス行全体を border 文字に置換
    // {MkdrCodeFence} マークアップで明示的な色を適用し、
    // markdown.kak のシンタックスハイライトを上書きする
    let ch = ctx.config.code_fence_char;
    let border_chars: String = std::iter::repeat(escape_markup(&ch.to_string()))
        .take(ctx.window_width)
        .collect();
    let border = format!("{{MkdrCodeFence}}{border_chars}");

    let open_end_offset = range.start + open_line_end.saturating_sub(1);
    let (_, open_col_e) = byte_to_line_col(ctx.starts, open_end_offset.max(range.start));

    conceal.push(KakRange {
        line_start: open_line,
        col_start:  open_col,
        line_end:   open_line,
        col_end:    open_col_e,
        text:       border.clone(),
    });

    // コードブロック全体に MkdrCodeBlock フェイス適用
    let (close_line, close_col_e) =
        byte_to_line_col(ctx.starts, range.end.saturating_sub(1));

    faces.push(KakRange {
        line_start: open_line,
        col_start:  open_col,
        line_end:   close_line,
        col_end:    close_col_e,
        text:       "MkdrCodeBlock".to_string(),
    });

    // 言語ラベルがある場合: MkdrCodeLang フェイス適用
    if !lang.is_empty() {
        let lang_start = range.start + fence_len;
        let lang_end   = lang_start + lang.len().saturating_sub(1);
        if lang_end >= lang_start {
            let (ll_s, lc_s) = byte_to_line_col(ctx.starts, lang_start);
            let (ll_e, lc_e) = byte_to_line_col(ctx.starts, lang_end);
            faces.push(KakRange {
                line_start: ll_s,
                col_start:  lc_s,
                line_end:   ll_e,
                col_end:    lc_e,
                text:       "MkdrCodeLang".to_string(),
            });
        }
    }

    // 閉じフェンス行を border に置換（最後の行）
    // 最後の \n の前を探す
    let content_without_trailing = if src.ends_with('\n') {
        &src[..src.len() - 1]
    } else {
        src
    };
    let close_fence_start_in_src = content_without_trailing
        .rfind('\n')
        .map(|p| p + 1)
        .unwrap_or(0);

    if close_fence_start_in_src > 0 {
        let close_abs = range.start + close_fence_start_in_src;
        let close_end_abs = range.start + content_without_trailing.len().saturating_sub(1);
        let (cl_s, cc_s) = byte_to_line_col(ctx.starts, close_abs);
        let (cl_e, cc_e) = byte_to_line_col(ctx.starts, close_end_abs.max(close_abs));

        conceal.push(KakRange {
            line_start: cl_s,
            col_start:  cc_s,
            line_end:   cl_e,
            col_end:    cc_e,
            text:       border,
        });
    }
}
