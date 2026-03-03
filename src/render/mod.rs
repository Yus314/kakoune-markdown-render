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
