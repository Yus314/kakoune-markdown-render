use crate::kak::KakRange;
use crate::offset::byte_to_line_col;
use super::RenderCtx;

pub fn render(
    range:   std::ops::Range<usize>,
    ctx:     &RenderCtx<'_>,
    conceal: &mut Vec<KakRange>,
    faces:   &mut Vec<KakRange>,
) {
    // `~~text~~` 全体の range
    // `~~` マーカー（2バイト固定）を空文字列に置換（conceal）し、テキスト部にフェイス適用

    let (line_s, col_s) = byte_to_line_col(ctx.starts, range.start);
    let (line_e, col_e) = byte_to_line_col(ctx.starts, range.end.saturating_sub(1));

    // 開き `~~` を空文字列に置換
    let open_end = range.start + 1;  // 2バイト (~~) の最後のバイト位置
    let (ol_e, oc_e) = byte_to_line_col(ctx.starts, open_end);
    conceal.push(KakRange {
        line_start: line_s,
        col_start:  col_s,
        line_end:   ol_e,
        col_end:    oc_e,
        text:       String::new(),
    });

    // 閉じ `~~` を空文字列に置換（末尾2バイト）
    if range.end >= 2 {
        let close_start = range.end - 2;
        let close_end   = range.end - 1;
        if close_start > range.start + 1 {
            let (cl_s, cc_s) = byte_to_line_col(ctx.starts, close_start);
            let (cl_e, cc_e) = byte_to_line_col(ctx.starts, close_end);
            conceal.push(KakRange {
                line_start: cl_s,
                col_start:  cc_s,
                line_end:   cl_e,
                col_end:    cc_e,
                text:       String::new(),
            });
        }
    }

    // テキスト全体に MkdrStrikethrough フェイス
    faces.push(KakRange {
        line_start: line_s,
        col_start:  col_s,
        line_end:   line_e,
        col_end:    col_e,
        text:       "MkdrStrikethrough".to_string(),
    });
}
