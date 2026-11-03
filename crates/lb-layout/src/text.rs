//! White-space processing and greedy line breaking at spaces.

use lb_css::WhiteSpace;

use crate::fonts::TextFont;

/// Collapses white space per `white-space`. `strip_leading` drops a
/// leading collapsible space (the previous inline content ended in one).
pub(crate) fn collapse(text: &str, ws: WhiteSpace, strip_leading: bool) -> String {
    let mut out = String::with_capacity(text.len());
    if !ws.collapses_spaces() {
        // pre / pre-wrap / break-spaces: keep everything, tabs as 8 spaces.
        for c in text.chars() {
            if c == '\t' {
                out.push_str("        ");
            } else if c != '\r' {
                out.push(c);
            }
        }
        return out;
    }
    let keep_newlines = ws.preserves_newlines();
    let mut pending_space = false;
    let space_ok = |out: &String| !(out.is_empty() && strip_leading) && !out.ends_with('\n');
    for c in text.chars() {
        match c {
            '\n' if keep_newlines => {
                // pre-line: spaces around a newline are removed.
                pending_space = false;
                out.push('\n');
            }
            ' ' | '\t' | '\n' | '\r' | '\x0c' => pending_space = true,
            c => {
                if pending_space && space_ok(&out) {
                    out.push(' ');
                }
                pending_space = false;
                out.push(c);
            }
        }
    }
    if pending_space && space_ok(&out) {
        out.push(' ');
    }
    out
}

/// A text run prepared for line breaking: words with their widths.
#[derive(Clone, Debug, Default)]
pub(crate) struct TextRun {
    pub(crate) text: String,
    words: Vec<Word>,
    pub(crate) line_height: f32,
    wraps: bool,
    font: Option<TextFont>,
    ws: Option<WhiteSpace>,
}

#[derive(Clone, Debug)]
struct Word {
    /// Width without the trailing space.
    width: f32,
    /// Width of the trailing space (0 if none).
    space: f32,
    /// A forced line break follows.
    newline: bool,
}

/// Result of breaking a run into lines.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Lines {
    pub(crate) count: usize,
    pub(crate) max_width: f32,
}

impl TextRun {
    pub(crate) fn new(text: String, font: &TextFont, line_height: f32, ws: WhiteSpace) -> TextRun {
        let mut words = Vec::new();
        let mut cur = String::new();
        let flush = |cur: &mut String, words: &mut Vec<Word>, space: f32, newline: bool| {
            words.push(Word {
                width: font.measure(cur),
                space,
                newline,
            });
            cur.clear();
        };
        let space_w = font.measure(" ");
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\n' => flush(&mut cur, &mut words, 0.0, true),
                ' ' if ws.wraps() => {
                    // Each space ends a word; a run of preserved spaces
                    // becomes extra width on the last word.
                    let mut n = 1.0;
                    while chars.peek() == Some(&' ') {
                        chars.next();
                        n += 1.0;
                    }
                    flush(&mut cur, &mut words, space_w * n, false);
                }
                c => cur.push(c),
            }
        }
        if !cur.is_empty() || words.is_empty() {
            flush(&mut cur, &mut words, 0.0, false);
        }
        TextRun {
            text,
            words,
            line_height,
            wraps: ws.wraps(),
            font: Some(font.clone()),
            ws: Some(ws),
        }
    }

    /// Replaces the text (e.g. after trimming a trailing space).
    pub(crate) fn trim_to(&mut self, text: String) {
        if let (Some(font), Some(ws)) = (self.font.take(), self.ws) {
            *self = TextRun::new(text, &font, self.line_height, ws);
        }
    }

    /// Widest unbreakable piece (min-content width).
    pub(crate) fn min_content(&self) -> f32 {
        if !self.wraps {
            return self.layout(None).max_width;
        }
        self.words.iter().map(|w| w.width).fold(0.0, f32::max)
    }

    /// Breaks into lines no wider than `max` (unless a word is wider).
    pub(crate) fn layout(&self, max: Option<f32>) -> Lines {
        let max = if self.wraps { max } else { None };
        let mut count = 1;
        let mut max_width: f32 = 0.0;
        // Width of the line so far, excluding a trailing space.
        let mut line: f32 = 0.0;
        let mut trailing_space: f32 = 0.0;
        let mut empty = true;
        for w in &self.words {
            if !empty
                && let Some(m) = max
                && line + trailing_space + w.width > m + 0.01
            {
                max_width = max_width.max(line);
                count += 1;
                line = 0.0;
                trailing_space = 0.0;
            }
            line += trailing_space + w.width;
            trailing_space = w.space;
            empty = false;
            if w.newline {
                max_width = max_width.max(line);
                count += 1;
                line = 0.0;
                trailing_space = 0.0;
                empty = true;
            }
        }
        // A trailing space left on the last line is followed by more inline
        // content (run ends are trimmed), so it takes up room.
        max_width = max_width.max(line + trailing_space);
        Lines { count, max_width }
    }

    /// Size for an available width (`None` = max-content).
    pub(crate) fn size(&self, max: Option<f32>) -> (f32, f32) {
        let lines = self.layout(max);
        (lines.max_width, lines.count as f32 * self.line_height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_white_space() {
        assert_eq!(collapse("  a \n\t b  ", WhiteSpace::Normal, false), " a b ");
        assert_eq!(collapse("  a  b", WhiteSpace::Normal, true), "a b");
        assert_eq!(collapse("a\n b", WhiteSpace::PreLine, false), "a\nb");
        assert_eq!(collapse("a  \tb", WhiteSpace::Pre, false), "a          b");
    }
}
