/// バッファ内の各行の開始バイトオフセット（0-indexed）
pub fn line_starts(content: &str) -> Vec<usize> {
    let mut s = vec![0];
    for (i, b) in content.bytes().enumerate() {
        if b == b'\n' {
            s.push(i + 1);
        }
    }
    s
}

/// バイトオフセット → (行, 列) 変換（1-based）
/// pulldown-cmark の Range.end は排他的。Kakoune は包含的。
/// → end 側は byte_to_line_col(starts, range.end - 1) を使うこと。
pub fn byte_to_line_col(starts: &[usize], offset: usize) -> (usize, usize) {
    let line = starts.partition_point(|&s| s <= offset) - 1;
    (line + 1, offset - starts[line] + 1)
}
