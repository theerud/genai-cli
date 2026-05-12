use std::io::Write;
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Style, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::as_24_bit_terminal_escaped;

pub trait Renderer {
    fn push(&mut self, chunk: &str);
    fn finish(&mut self);
}

pub fn pick_style(tty: bool, color: Option<bool>, markdown: Option<bool>) -> RenderStyle {
    if !tty {
        return RenderStyle::Plain;
    }
    if markdown.unwrap_or(true) {
        return RenderStyle::Markdown;
    }
    if color.unwrap_or(true) {
        return RenderStyle::Color;
    }
    RenderStyle::Plain
}

pub fn make_boxed<W: Write + 'static>(
    out: W,
    tty: bool,
    style: RenderStyle,
) -> Box<dyn Renderer> {
    match style {
        RenderStyle::Plain => Box::new(PlainRenderer::new(out, tty)),
        RenderStyle::Color => Box::new(ColorRenderer::new(out, tty)),
        RenderStyle::Markdown => Box::new(MarkdownRenderer::new(out, tty)),
    }
}

#[derive(Copy, Clone, Debug)]
pub enum RenderStyle {
    Plain,
    Color,
    Markdown,
}

pub struct PlainRenderer<W: Write> {
    out: W,
    tty: bool,
}

impl<W: Write> PlainRenderer<W> {
    pub fn new(out: W, tty: bool) -> Self {
        Self { out, tty }
    }
}

impl<W: Write> Renderer for PlainRenderer<W> {
    fn push(&mut self, chunk: &str) {
        let _ = self.out.write_all(chunk.as_bytes());
        if self.tty {
            let _ = self.out.flush();
        }
    }
    fn finish(&mut self) {
        let _ = self.out.write_all(b"\n");
        let _ = self.out.flush();
    }
}

// ---------- syntect bootstrap ----------

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME: OnceLock<Theme> = OnceLock::new();

fn syntax_set() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn theme() -> &'static Theme {
    THEME.get_or_init(|| {
        let ts = ThemeSet::load_defaults();
        ts.themes
            .get("base16-ocean.dark")
            .cloned()
            .unwrap_or_else(|| ts.themes.values().next().cloned().expect("a theme"))
    })
}

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const ITAL: &str = "\x1b[3m";
const CYAN: &str = "\x1b[36m";
const MAGENTA: &str = "\x1b[35m";
const YELLOW: &str = "\x1b[33m";

// ---------- ColorRenderer ----------
// Plain text passthrough, except fenced code blocks are buffered and
// highlighted with syntect on close.

pub struct ColorRenderer<W: Write> {
    out: W,
    tty: bool,
    line_buf: String,
    in_code: bool,
    code_lang: String,
    code_buf: String,
}

impl<W: Write> ColorRenderer<W> {
    pub fn new(out: W, tty: bool) -> Self {
        Self {
            out,
            tty,
            line_buf: String::new(),
            in_code: false,
            code_lang: String::new(),
            code_buf: String::new(),
        }
    }

    fn emit_line(&mut self, line: &str) {
        let trimmed = line.trim_start();

        if let Some(rest) = trimmed.strip_prefix("```") {
            if !self.in_code {
                self.in_code = true;
                self.code_lang = rest.trim().to_string();
                self.code_buf.clear();
                if self.tty {
                    let _ = writeln!(self.out, "{DIM}┌── {}{RESET}", self.code_lang);
                } else {
                    let _ = writeln!(self.out, "```{}", self.code_lang);
                }
                return;
            }
            // closing fence
            self.flush_code();
            self.in_code = false;
            if self.tty {
                let _ = writeln!(self.out, "{DIM}└──{RESET}");
            } else {
                let _ = writeln!(self.out, "```");
            }
            return;
        }

        if self.in_code {
            self.code_buf.push_str(line);
            self.code_buf.push('\n');
            return;
        }

        let _ = self.out.write_all(line.as_bytes());
        let _ = self.out.write_all(b"\n");
    }

    fn flush_code(&mut self) {
        if !self.tty {
            let _ = self.out.write_all(self.code_buf.as_bytes());
            return;
        }
        let ss = syntax_set();
        let syntax = if self.code_lang.is_empty() {
            ss.find_syntax_plain_text()
        } else {
            ss.find_syntax_by_token(&self.code_lang)
                .or_else(|| ss.find_syntax_by_extension(&self.code_lang))
                .or_else(|| ss.find_syntax_by_name(&self.code_lang))
                .unwrap_or_else(|| ss.find_syntax_plain_text())
        };
        let mut h = HighlightLines::new(syntax, theme());
        for line in self.code_buf.lines() {
            match h.highlight_line(line, ss) {
                Ok(ranges) => {
                    let escaped: Vec<(Style, &str)> = ranges;
                    let s = as_24_bit_terminal_escaped(&escaped, false);
                    let _ = self.out.write_all(s.as_bytes());
                    let _ = self.out.write_all(RESET.as_bytes());
                    let _ = self.out.write_all(b"\n");
                }
                Err(_) => {
                    let _ = writeln!(self.out, "{line}");
                }
            }
        }
        self.code_buf.clear();
    }
}

impl<W: Write> Renderer for ColorRenderer<W> {
    fn push(&mut self, chunk: &str) {
        self.line_buf.push_str(chunk);
        while let Some(idx) = self.line_buf.find('\n') {
            let line: String = self.line_buf[..idx].to_string();
            self.line_buf.drain(..=idx);
            self.emit_line(&line);
            if self.tty {
                let _ = self.out.flush();
            }
        }
    }

    fn finish(&mut self) {
        if !self.line_buf.is_empty() {
            let line = std::mem::take(&mut self.line_buf);
            self.emit_line(&line);
        }
        if self.in_code {
            self.flush_code();
            if self.tty {
                let _ = writeln!(self.out, "{DIM}└── (unterminated){RESET}");
            }
            self.in_code = false;
        }
        let _ = self.out.flush();
    }
}

// ---------- MarkdownRenderer ----------
// Line-state machine: headings, bullets, blockquotes; inline bold/italic/code.
// Fenced code uses syntect via the same path as ColorRenderer.

pub struct MarkdownRenderer<W: Write> {
    inner: ColorRenderer<W>,
}

impl<W: Write> MarkdownRenderer<W> {
    pub fn new(out: W, tty: bool) -> Self {
        Self {
            inner: ColorRenderer::new(out, tty),
        }
    }

    fn render_line(&mut self, line: &str) {
        let trimmed = line.trim_start();

        if trimmed.starts_with("```") {
            self.inner.emit_line(line);
            return;
        }
        if self.inner.in_code {
            self.inner.emit_line(line);
            return;
        }
        if !self.inner.tty {
            let _ = self.inner.out.write_all(line.as_bytes());
            let _ = self.inner.out.write_all(b"\n");
            return;
        }

        let indent_len = line.len() - trimmed.len();
        let indent = &line[..indent_len];

        // Headings
        if let Some(rest) = trimmed.strip_prefix("###### ") {
            let _ = writeln!(self.inner.out, "{indent}{BOLD}{}{RESET}", inline(rest));
            return;
        }
        if let Some(rest) = trimmed.strip_prefix("##### ") {
            let _ = writeln!(self.inner.out, "{indent}{BOLD}{}{RESET}", inline(rest));
            return;
        }
        if let Some(rest) = trimmed.strip_prefix("#### ") {
            let _ = writeln!(self.inner.out, "{indent}{BOLD}{MAGENTA}{}{RESET}", inline(rest));
            return;
        }
        if let Some(rest) = trimmed.strip_prefix("### ") {
            let _ = writeln!(self.inner.out, "{indent}{BOLD}{MAGENTA}{}{RESET}", inline(rest));
            return;
        }
        if let Some(rest) = trimmed.strip_prefix("## ") {
            let _ = writeln!(self.inner.out, "{indent}{BOLD}{CYAN}{}{RESET}", inline(rest));
            return;
        }
        if let Some(rest) = trimmed.strip_prefix("# ") {
            let _ = writeln!(self.inner.out, "{indent}{BOLD}{CYAN}{}{RESET}", inline(rest));
            return;
        }

        // Blockquote
        if let Some(rest) = trimmed.strip_prefix("> ") {
            let _ = writeln!(self.inner.out, "{indent}{DIM}│ {}{RESET}", inline(rest));
            return;
        }

        // Bullets
        if let Some(rest) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            let _ = writeln!(self.inner.out, "{indent}{YELLOW}•{RESET} {}", inline(rest));
            return;
        }
        if let Some((marker, rest)) = numbered_bullet(trimmed) {
            let _ = writeln!(self.inner.out, "{indent}{YELLOW}{marker}{RESET} {}", inline(rest));
            return;
        }

        let _ = writeln!(self.inner.out, "{}", inline(line));
    }
}

impl<W: Write> Renderer for MarkdownRenderer<W> {
    fn push(&mut self, chunk: &str) {
        self.inner.line_buf.push_str(chunk);
        while let Some(idx) = self.inner.line_buf.find('\n') {
            let line: String = self.inner.line_buf[..idx].to_string();
            self.inner.line_buf.drain(..=idx);
            self.render_line(&line);
            if self.inner.tty {
                let _ = self.inner.out.flush();
            }
        }
    }
    fn finish(&mut self) {
        if !self.inner.line_buf.is_empty() {
            let line = std::mem::take(&mut self.inner.line_buf);
            self.render_line(&line);
        }
        if self.inner.in_code {
            self.inner.flush_code();
            self.inner.in_code = false;
        }
        let _ = self.inner.out.flush();
    }
}

fn numbered_bullet(s: &str) -> Option<(String, &str)> {
    let mut chars = s.char_indices();
    let mut digit_end = 0;
    let mut has_digit = false;
    for (i, c) in chars.by_ref() {
        if c.is_ascii_digit() {
            digit_end = i + 1;
            has_digit = true;
        } else {
            digit_end = i;
            break;
        }
    }
    if !has_digit {
        return None;
    }
    let after_digits = &s[digit_end..];
    let rest = after_digits.strip_prefix(". ").or_else(|| after_digits.strip_prefix(") "))?;
    let marker = format!("{}.", &s[..digit_end]);
    Some((marker, rest))
}

fn inline(s: &str) -> String {
    // **bold**, *italic* (or _italic_), `code` — non-nested, greedy-pair.
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'*' && i + 1 < bytes.len() && bytes[i + 1] == b'*'
            && let Some(end) = find_pair(s, i + 2, "**") {
                out.push_str(BOLD);
                out.push_str(&s[i + 2..end]);
                out.push_str(RESET);
                i = end + 2;
                continue;
            }
        if (bytes[i] == b'*' || bytes[i] == b'_') && !is_word_boundary(s, i) {
            // skip — likely inside a word
        } else if bytes[i] == b'*' || bytes[i] == b'_' {
            let marker = &s[i..i + 1];
            if let Some(end) = find_pair(s, i + 1, marker)
                && end > i + 1 {
                    out.push_str(ITAL);
                    out.push_str(&s[i + 1..end]);
                    out.push_str(RESET);
                    i = end + 1;
                    continue;
                }
        }
        if bytes[i] == b'`'
            && let Some(end) = find_pair(s, i + 1, "`") {
                out.push_str(YELLOW);
                out.push_str(&s[i + 1..end]);
                out.push_str(RESET);
                i = end + 1;
                continue;
            }
        out.push(s[i..].chars().next().unwrap());
        i += s[i..].chars().next().unwrap().len_utf8();
    }
    out
}

fn find_pair(s: &str, from: usize, needle: &str) -> Option<usize> {
    s[from..].find(needle).map(|p| from + p)
}

fn is_word_boundary(s: &str, i: usize) -> bool {
    let prev = s[..i].chars().last();
    match prev {
        None => true,
        Some(c) => !c.is_alphanumeric(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_color(input: &str) -> String {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut r = ColorRenderer::new(&mut buf, false);
            r.push(input);
            r.finish();
        }
        String::from_utf8(buf).unwrap()
    }

    fn render_md(input: &str) -> String {
        let mut buf: Vec<u8> = Vec::new();
        {
            let mut r = MarkdownRenderer::new(&mut buf, false);
            r.push(input);
            r.finish();
        }
        String::from_utf8(buf).unwrap()
    }

    #[test]
    fn color_passes_plain_text_through() {
        assert_eq!(render_color("hello world\n"), "hello world\n");
    }

    #[test]
    fn color_keeps_fences_in_non_tty() {
        let out = render_color("```rust\nfn main() {}\n```\n");
        assert!(out.contains("```rust"));
        assert!(out.contains("fn main"));
    }

    #[test]
    fn markdown_non_tty_passes_through() {
        // tty=false should round-trip exactly (we keep raw markdown for pipes)
        assert_eq!(render_md("# title\n- a\n"), "# title\n- a\n");
    }
}
