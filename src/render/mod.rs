use std::borrow::Cow;

use pulldown_cmark::{Options, Parser, Event, Tag, HeadingLevel};

use crate::config::Config;
use crate::kak::KakRange;
use crate::offset::line_starts;

pub mod heading;
pub mod thematic;
pub mod blockquote;
pub mod list;
pub mod task;
pub mod code_block;
pub mod strikethrough;

/// 各モジュールに渡す共通コンテキスト
pub struct RenderCtx<'a> {
    pub content:      &'a str,
    pub starts:       &'a [usize],
    pub config:       &'a Config,
    pub window_width: usize,
}

pub struct Renderer<'a> {
    content:      &'a str,
    starts:       Vec<usize>,
    config:       &'a Config,
    window_width: usize,
}

impl<'a> Renderer<'a> {
    pub fn new(content: &'a str, config: &'a Config, window_width: usize) -> Self {
        Renderer {
            content,
            starts: line_starts(content),
            config,
            window_width,
        }
    }

    /// 未フィルタの KakRange を返す。フィルタは emit 直前に apply する。
    pub fn render_unfiltered(&self) -> (Vec<KakRange>, Vec<KakRange>) {
        let mut conceal: Vec<KakRange> = Vec::new();
        let mut faces:   Vec<KakRange> = Vec::new();

        let ctx = RenderCtx {
            content:      self.content,
            starts:       &self.starts,
            config:       self.config,
            window_width: self.window_width,
        };

        let opts = Options::ENABLE_TABLES
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_SMART_PUNCTUATION;

        // (is_ordered, start_num) のスタック。ネスト対応。
        let mut list_stack: Vec<(bool, u64)> = Vec::new();

        // into_offset_iter() は (Event, Range<usize>) を返す。
        // event を by value でマッチして所有権の複雑さを避ける（Range<usize> は Copy）。
        for (event, range) in Parser::new_ext(self.content, opts).into_offset_iter() {
            match event {
                Event::Start(Tag::List(start)) => {
                    list_stack.push((start.is_some(), start.unwrap_or(1)));
                }
                Event::End(Tag::List(_)) => {
                    list_stack.pop();
                }

                // pulldown-cmark 0.9: Tag::Heading(level, id, classes)
                Event::Start(Tag::Heading(level, _, _)) => {
                    let level_num = heading_level_to_usize(level);
                    heading::render(range, level_num, &ctx, &mut conceal, &mut faces);
                }

                Event::Rule =>
                    thematic::render(range, &ctx, &mut conceal, &mut faces),

                // pulldown-cmark 0.9: Tag::BlockQuote (引数なし)
                Event::Start(Tag::BlockQuote) =>
                    blockquote::render(range, &ctx, &mut conceal, &mut faces),

                Event::Start(Tag::Item) => {
                    let depth = list_stack.len().saturating_sub(1);
                    let (is_ordered, start_num) =
                        list_stack.last().copied().unwrap_or((false, 1));
                    list::render(
                        range, depth, is_ordered, start_num,
                        &ctx, &mut conceal, &mut faces,
                    );
                }

                Event::TaskListMarker(checked) =>
                    task::render(range, checked, &ctx, &mut conceal, &mut faces),

                Event::Start(Tag::CodeBlock(_)) =>
                    code_block::render(range, &ctx, &mut conceal, &mut faces),

                Event::Start(Tag::Strikethrough) =>
                    strikethrough::render(range, &ctx, &mut conceal, &mut faces),

                // M7 以降: emphasis/strong/code_span/link は別途追加
                _ => {}
            }
        }

        (conceal, faces)
    }
}

/// HeadingLevel を 1-indexed usize に変換する。
fn heading_level_to_usize(level: HeadingLevel) -> usize {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn debug_ranges(label: &str, ranges: &[KakRange]) {
        for r in ranges {
            eprintln!("  {label}: {}.{},{}.{}|{}", r.line_start, r.col_start, r.line_end, r.col_end, r.text);
        }
    }

    #[test]
    fn pulldown_strikethrough_events() {
        let content = "~~hello~~\n";
        let opts = Options::ENABLE_STRIKETHROUGH;
        let events: Vec<_> = Parser::new_ext(content, opts)
            .into_offset_iter()
            .collect();
        let mut report = String::new();
        for (ev, range) in &events {
            report.push_str(&format!("{:?} range={:?} src={:?}\n", ev, range, &content[range.clone()]));
        }
        // Verify Strikethrough event exists
        let has_strike = events.iter().any(|(ev, _)| matches!(ev, Event::Start(Tag::Strikethrough)));
        assert!(has_strike, "ENABLE_STRIKETHROUGH should produce Start(Strikethrough). Events:\n{}", report);

        // Verify range covers full ~~hello~~ (0..9)
        let (_, strike_range) = events.iter()
            .find(|(ev, _)| matches!(ev, Event::Start(Tag::Strikethrough)))
            .unwrap();
        assert_eq!(strike_range, &(0..9), "Strikethrough range should cover full ~~hello~~");
    }

    #[test]
    fn render_strikethrough() {
        let content = "~~hello~~\n";
        let config = Config::default();
        let r = Renderer::new(content, &config, 80);
        let (conceal, faces) = r.render_unfiltered();
        debug_ranges("conceal", &conceal);
        debug_ranges("faces", &faces);
        // Opening ~~ and closing ~~ should be concealed
        let empty_conceals: Vec<_> = conceal.iter().filter(|r| r.text.is_empty()).collect();
        assert!(empty_conceals.len() >= 2,
            "expected 2 empty conceal ranges for ~~, got {}: {:?}",
            empty_conceals.len(),
            empty_conceals.iter().map(|r| format!("{}.{},{}.{}", r.line_start, r.col_start, r.line_end, r.col_end)).collect::<Vec<_>>()
        );
        // MkdrStrikethrough face should be applied
        let strike_faces: Vec<_> = faces.iter().filter(|r| r.text == "MkdrStrikethrough").collect();
        assert_eq!(strike_faces.len(), 1, "expected 1 MkdrStrikethrough face");
    }

    #[test]
    fn render_thematic_break() {
        let content = "text\n\n---\n";
        let config = Config::default();
        let r = Renderer::new(content, &config, 80);
        let (conceal, faces) = r.render_unfiltered();
        debug_ranges("conceal", &conceal);
        debug_ranges("faces", &faces);
        // Thematic break should generate 1 conceal (--- → ─×80) and 1 face
        let thematic_conceals: Vec<_> = conceal.iter()
            .filter(|r| r.text.contains('─'))
            .collect();
        assert_eq!(thematic_conceals.len(), 1, "expected 1 thematic conceal");
        assert_eq!(thematic_conceals[0].line_start, 3, "thematic break should be on line 3");
    }

    #[test]
    fn format_commands_with_strikethrough() {
        use crate::kak::format_commands;
        let content = "~~hello~~\n";
        let config = Config::default();
        let r = Renderer::new(content, &config, 80);
        let (conceal, faces) = r.render_unfiltered();
        let cmd = format_commands("client0", "test.md", 42, 80, &conceal, &faces, 0xabcd);
        // Conceal specs for opening and closing ~~
        assert!(cmd.contains("'1.1,1.2|'"), "should conceal opening ~~");
        assert!(cmd.contains("'1.8,1.9|'"), "should conceal closing ~~");
        assert!(cmd.contains("'1.1,1.9|MkdrStrikethrough'"), "should have strikethrough face");
    }

    #[test]
    fn indented_code_block_closing() {
        // 2スペースインデントされたコードブロック
        let content = "  ```python\n  print(\"hello\")\n  ```\n";
        let opts = Options::ENABLE_TABLES
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_STRIKETHROUGH;
        let events: Vec<_> = Parser::new_ext(content, opts)
            .into_offset_iter()
            .collect();
        let mut report = String::new();
        for (ev, range) in &events {
            report.push_str(&format!("{:?} range={:?} src={:?}\n", ev, range, &content[range.clone()]));
        }
        // Code block should close, not extend to EOF
        let code_starts: Vec<_> = events.iter()
            .filter(|(ev, _)| matches!(ev, Event::Start(Tag::CodeBlock(_))))
            .collect();
        assert_eq!(code_starts.len(), 1, "expected 1 code block, events:\n{}", report);
        let (_, range) = &code_starts[0];
        // Range should NOT extend to the end of the content
        assert!(range.end <= content.len(),
            "code block range {:?} should be within content (len={}), events:\n{}",
            range, content.len(), report);
    }

    #[test]
    fn indented_content_like_user_file() {
        // ユーザファイルと同じ構造: 2スペースインデント
        let content = "\
  # Heading 1

  ## Heading 2

  ---

  > Quote

  - Item A

  ```python
  print(\"hello\")
  ```

  ~~strike~~

  ---
";
        let opts = Options::ENABLE_TABLES
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_STRIKETHROUGH;
        let events: Vec<_> = Parser::new_ext(content, opts)
            .into_offset_iter()
            .collect();
        let mut report = String::new();
        for (ev, range) in &events {
            report.push_str(&format!("{:?} range={:?} src={:?}\n", ev, range, &content[range.clone()]));
        }
        // Strikethrough should be present
        let has_strike = events.iter()
            .any(|(ev, _)| matches!(ev, Event::Start(Tag::Strikethrough)));
        // Last Rule should be present (2nd thematic break)
        let rule_count = events.iter()
            .filter(|(ev, _)| matches!(ev, Event::Rule))
            .count();
        assert!(has_strike, "expected Strikethrough event, events:\n{}", report);
        assert_eq!(rule_count, 2, "expected 2 Rule events, events:\n{}", report);
    }

    #[test]
    fn render_full_test_doc() {
        let content = "\
# Heading 1

## Heading 2

---

> Quote

- Item A
  - Nested

- [ ] Unchecked
- [x] Checked

```python
print(\"hello\")
```

~~strike~~
";
        let config = Config::default();
        let r = Renderer::new(content, &config, 80);
        let (conceal, faces) = r.render_unfiltered();
        eprintln!("=== CONCEAL ({}) ===", conceal.len());
        debug_ranges("C", &conceal);
        eprintln!("=== FACES ({}) ===", faces.len());
        debug_ranges("F", &faces);

        // Basic sanity: we should have non-zero output
        assert!(!conceal.is_empty(), "conceal should not be empty");
        assert!(!faces.is_empty(), "faces should not be empty");
    }
}

/// emit 直前にカーソル近傍を除外する。
/// context=0 の場合は Cow::Borrowed でゼロコピー返却。
/// context>0 の場合は Cow::Owned でフィルタ済み新 Vec を返す。
pub fn filter_cursor_overlap<'a>(
    ranges:      &'a [KakRange],
    cursor_line: usize,
    context:     usize,
) -> Cow<'a, [KakRange]> {
    // context=0 はフィルタなし（全 range を通す）
    if context == 0 { return Cow::Borrowed(ranges); }
    let lo = cursor_line.saturating_sub(context);
    let hi = cursor_line + context;
    Cow::Owned(
        ranges.iter()
            .filter(|r| r.line_end < lo || r.line_start > hi)
            .cloned()
            .collect()
    )
}
