//! Agent Markdown as a small, safe document tree.
//!
//! Agents (the Queen and helpers) write Markdown. [`parse`] reads it as CommonMark plus the GFM
//! extensions (tables, strikethrough, task lists) with `pulldown-cmark` and folds the events into
//! a [`Node`] tree that `components::markdown` renders with maud. This module has no markup of
//! its own; every text in the tree is escaped when rendered.
//!
//! Safety rules (agent text is untrusted):
//! - Raw HTML (block and inline) becomes plain text.
//! - Links keep only `http`, `https` and `mailto` targets ([`safe_href`]); anything else
//!   (`javascript:`, `data:`, relative paths...) leaves just the link text.
//! - Images are never loaded: they become a link with the alt text.
//! - Bare `http(s)://` URLs in text become links ([`linkify`]), but not inside code or links.
//! - Nesting is capped at [`MAX_DEPTH`] so rendering (recursive) stays bounded.

use pulldown_cmark::{
    Alignment, Event, HeadingLevel, LinkType, Options, Parser, Tag, TagEnd, TextMergeStream,
};

/// Deeper nesting than this is flattened (the content stays, the extra wrappers go).
pub const MAX_DEPTH: usize = 24;

/// Column alignment in a table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    None,
    Left,
    Center,
    Right,
}

/// A container element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum El {
    Para,
    /// 1..=6, as written; the renderer scales it down for chat.
    Heading(u8),
    Quote,
    List {
        /// `Some(n)`: an ordered list starting at `n`.
        start: Option<u64>,
    },
    Item,
    Table,
    TableHead,
    TableRow,
    TableCell {
        head: bool,
        align: Align,
    },
    Emphasis,
    Strong,
    Strike,
}

/// One node of the document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Text(String),
    /// Inline code.
    Code(String),
    /// A fenced or indented code block (its whole text).
    CodeBlock(String),
    /// A link to a checked target ([`safe_href`]). `image`: it was an image (not loaded).
    Link {
        href: String,
        image: bool,
        children: Vec<Node>,
    },
    Break,
    Rule,
    /// A task list checkbox (`- [x]`).
    Task(bool),
    El(El, Vec<Node>),
}

/// What an open frame becomes when it ends.
enum Frame {
    El(El),
    /// A link or image; `None` when the target was refused (only the text stays).
    Link {
        href: Option<String>,
        image: bool,
    },
    /// No wrapper (too deep, raw HTML block, or a construct not shown).
    Transparent,
    CodeBlock(String),
}

/// Parses agent Markdown into a safe tree.
pub fn parse(text: &str) -> Vec<Node> {
    let opts = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut b = Builder::default();
    for ev in TextMergeStream::new(Parser::new_ext(text, opts)) {
        b.event(ev);
    }
    b.finish()
}

#[derive(Default)]
struct Builder {
    root: Vec<Node>,
    /// Open frames with their children so far.
    stack: Vec<(Frame, Vec<Node>)>,
    /// Real (non-transparent) nesting depth.
    depth: usize,
    /// Alignments of the table being read, and the current column.
    aligns: Vec<Alignment>,
    column: usize,
    in_head: bool,
    /// Inside a link or image: no linkifying.
    in_link: usize,
}

impl Builder {
    fn children(&mut self) -> &mut Vec<Node> {
        match self.stack.last_mut() {
            Some((_, c)) => c,
            None => &mut self.root,
        }
    }

    fn push(&mut self, n: Node) {
        // Merge adjacent text (e.g. raw HTML pieces) to keep the tree small.
        if let (Node::Text(t), Some(Node::Text(prev))) = (&n, self.children().last_mut()) {
            prev.push_str(t);
            return;
        }
        self.children().push(n);
    }

    fn open(&mut self, f: Frame) {
        let f = match f {
            Frame::Transparent => Frame::Transparent,
            _ if self.depth >= MAX_DEPTH => Frame::Transparent,
            f => {
                self.depth += 1;
                f
            }
        };
        if matches!(f, Frame::Link { .. }) {
            self.in_link += 1;
        }
        self.stack.push((f, Vec::new()));
    }

    fn close(&mut self) {
        let Some((frame, children)) = self.stack.pop() else {
            return;
        };
        match frame {
            Frame::Transparent => {
                for c in children {
                    self.push(c);
                }
            }
            Frame::El(el) => {
                self.depth -= 1;
                self.push(Node::El(el, children));
            }
            Frame::CodeBlock(code) => {
                self.depth -= 1;
                self.push(Node::CodeBlock(code));
            }
            Frame::Link { href, image } => {
                self.depth -= 1;
                self.in_link -= 1;
                match href {
                    Some(href) => {
                        let children = if children.is_empty() {
                            vec![Node::Text(href.clone())]
                        } else {
                            children
                        };
                        self.push(Node::Link {
                            href,
                            image,
                            children,
                        });
                    }
                    None => {
                        for c in children {
                            self.push(c);
                        }
                    }
                }
            }
        }
    }

    fn text(&mut self, t: &str) {
        if let Some((Frame::CodeBlock(code), _)) = self.stack.last_mut() {
            code.push_str(t);
            return;
        }
        if self.in_link > 0 {
            self.push(Node::Text(t.to_string()));
            return;
        }
        for seg in linkify(t) {
            match seg {
                Segment::Text(s) => self.push(Node::Text(s.to_string())),
                Segment::Url(u) => self.push(Node::Link {
                    href: u.to_string(),
                    image: false,
                    children: vec![Node::Text(u.to_string())],
                }),
            }
        }
    }

    /// Raw HTML: shown as written, as text.
    fn raw(&mut self, t: &str) {
        if let Some((Frame::CodeBlock(code), _)) = self.stack.last_mut() {
            code.push_str(t);
            return;
        }
        let mut lines = t.strip_suffix('\n').unwrap_or(t).split('\n').peekable();
        while let Some(line) = lines.next() {
            self.push(Node::Text(line.to_string()));
            if lines.peek().is_some() {
                self.push(Node::Break);
            }
        }
    }

    fn event(&mut self, ev: Event<'_>) {
        match ev {
            Event::Start(tag) => self.start(tag),
            Event::End(end) => {
                match end {
                    TagEnd::TableHead => self.in_head = false,
                    TagEnd::TableCell => self.column += 1,
                    TagEnd::Table => self.aligns.clear(),
                    _ => {}
                }
                self.close();
            }
            Event::Text(t) => self.text(&t),
            Event::Code(t) => self.push(Node::Code(t.to_string())),
            Event::Html(t) | Event::InlineHtml(t) => self.raw(&t),
            // Not enabled; kept as written.
            Event::InlineMath(t) | Event::DisplayMath(t) => self.push(Node::Text(t.to_string())),
            Event::FootnoteReference(t) => self.push(Node::Text(format!("[^{t}]"))),
            Event::SoftBreak | Event::HardBreak => self.push(Node::Break),
            Event::Rule => self.push(Node::Rule),
            Event::TaskListMarker(done) => self.push(Node::Task(done)),
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        let frame = match tag {
            Tag::Paragraph => Frame::El(El::Para),
            Tag::Heading { level, .. } => Frame::El(El::Heading(heading_level(level))),
            Tag::BlockQuote(_) => Frame::El(El::Quote),
            Tag::CodeBlock(_) => Frame::CodeBlock(String::new()),
            // Raw HTML block: its lines become text in a paragraph.
            Tag::HtmlBlock => Frame::El(El::Para),
            Tag::List(start) => Frame::El(El::List { start }),
            Tag::Item => Frame::El(El::Item),
            Tag::Table(aligns) => {
                self.aligns = aligns;
                self.column = 0;
                Frame::El(El::Table)
            }
            Tag::TableHead => {
                self.in_head = true;
                self.column = 0;
                Frame::El(El::TableHead)
            }
            Tag::TableRow => {
                self.column = 0;
                Frame::El(El::TableRow)
            }
            Tag::TableCell => Frame::El(El::TableCell {
                head: self.in_head,
                align: match self.aligns.get(self.column) {
                    Some(Alignment::Left) => Align::Left,
                    Some(Alignment::Center) => Align::Center,
                    Some(Alignment::Right) => Align::Right,
                    _ => Align::None,
                },
            }),
            Tag::Emphasis => Frame::El(El::Emphasis),
            Tag::Strong => Frame::El(El::Strong),
            Tag::Strikethrough => Frame::El(El::Strike),
            Tag::Link {
                link_type,
                dest_url,
                ..
            } => Frame::Link {
                href: safe_href(&dest_url, link_type),
                image: false,
            },
            Tag::Image {
                link_type,
                dest_url,
                ..
            } => Frame::Link {
                href: safe_href(&dest_url, link_type),
                image: true,
            },
            // Not enabled (footnotes, definition lists, super/subscript, metadata): content only.
            _ => Frame::Transparent,
        };
        self.open(frame);
    }

    fn finish(mut self) -> Vec<Node> {
        while !self.stack.is_empty() {
            self.close();
        }
        self.root
    }
}

fn heading_level(l: HeadingLevel) -> u8 {
    match l {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// The link target to use, or `None` when it is refused. Only absolute `http(s)://host...`
/// and `mailto:` targets pass; `<user@example.com>` autolinks become `mailto:`.
pub fn safe_href(url: &str, link_type: LinkType) -> Option<String> {
    let url = url.trim();
    if url.is_empty() || url.chars().any(char::is_control) {
        return None;
    }
    if link_type == LinkType::Email {
        return (!url.contains(':')).then(|| format!("mailto:{url}"));
    }
    let (scheme, rest) = url.split_once(':')?;
    let ok = if scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https") {
        rest.strip_prefix("//")
            .and_then(|r| r.chars().next())
            .is_some_and(|c| c.is_alphanumeric() || c == '[')
    } else if scheme.eq_ignore_ascii_case("mailto") {
        !rest.is_empty()
    } else {
        false
    };
    // Spaces (only possible in `<...>` targets) are encoded; the rest is escaped by maud.
    ok.then(|| url.replace(' ', "%20"))
}

/// A piece of text: plain, or a bare URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment<'a> {
    Text(&'a str),
    Url(&'a str),
}

/// Splits text into plain parts and bare `http://`/`https://` URLs. A URL starts at a word
/// boundary and runs to whitespace, `<`, `>`, `"` or a backtick; trailing punctuation
/// (`.,:;!?'*_~`) and unbalanced closing brackets are left out, as in GFM's autolinks.
pub fn linkify(text: &str) -> Vec<Segment<'_>> {
    let lower = text.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut last = 0;
    let mut from = 0;
    while let Some(found) = lower[from..].find("http") {
        let pos = from + found;
        from = pos + 4;
        let rest = &lower[pos..];
        let scheme_len = if rest.starts_with("https://") {
            8
        } else if rest.starts_with("http://") {
            7
        } else {
            continue;
        };
        if text[..pos]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '/' || c == '@')
        {
            continue;
        }
        let end = text[pos..]
            .find(|c: char| c.is_whitespace() || c.is_control() || "<>\"`".contains(c))
            .map_or(text.len(), |e| pos + e);
        let url = trim_url(&text[pos..end]);
        if !url[scheme_len..]
            .chars()
            .next()
            .is_some_and(char::is_alphanumeric)
        {
            continue;
        }
        if last < pos {
            out.push(Segment::Text(&text[last..pos]));
        }
        out.push(Segment::Url(url));
        last = pos + url.len();
        from = last;
    }
    if last < text.len() {
        out.push(Segment::Text(&text[last..]));
    }
    out
}

fn trim_url(mut url: &str) -> &str {
    loop {
        let Some(c) = url.chars().next_back() else {
            return url;
        };
        let drop = match c {
            '.' | ',' | ':' | ';' | '!' | '?' | '\'' | '*' | '_' | '~' => true,
            ')' => url.matches(')').count() > url.matches('(').count(),
            ']' => url.matches(']').count() > url.matches('[').count(),
            '}' => url.matches('}').count() > url.matches('{').count(),
            _ => false,
        };
        if !drop {
            return url;
        }
        url = &url[..url.len() - c.len_utf8()];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(text: &str) -> Vec<&str> {
        linkify(text)
            .into_iter()
            .filter_map(|s| match s {
                Segment::Url(u) => Some(u),
                Segment::Text(_) => None,
            })
            .collect()
    }

    #[test]
    fn linkify_finds_bare_urls() {
        assert_eq!(
            urls("Open http://nextcloud.example.com now"),
            ["http://nextcloud.example.com"]
        );
        assert_eq!(
            urls("See https://example.com/a?b=1&c=2#x."),
            ["https://example.com/a?b=1&c=2#x"]
        );
        assert_eq!(urls("(https://example.com/x)"), ["https://example.com/x"]);
        assert_eq!(
            urls("https://en.example.org/wiki/Foo_(bar), ok"),
            ["https://en.example.org/wiki/Foo_(bar)"]
        );
        assert_eq!(urls("HTTPS://EXAMPLE.COM!"), ["HTTPS://EXAMPLE.COM"]);
        assert_eq!(urls("<https://example.com>"), ["https://example.com"]);
        assert_eq!(
            urls("a \"https://example.com/q\" b"),
            ["https://example.com/q"]
        );
        assert_eq!(
            urls("two: http://a.example and https://b.example."),
            ["http://a.example", "https://b.example"]
        );
        assert_eq!(
            urls("http://203.0.113.5:8080/"),
            ["http://203.0.113.5:8080/"]
        );
    }

    #[test]
    fn linkify_skips_non_urls() {
        assert!(urls("no links here, just http and https").is_empty());
        assert!(urls("http:// alone, https://. too").is_empty());
        assert!(urls("xhttp://example.com").is_empty());
        assert!(urls("javascript:alert('http://x')").len() == 1);
        assert!(urls("ftp://example.com").is_empty());
    }

    #[test]
    fn linkify_keeps_all_text() {
        let t = "a http://x.example b. https://y.example/(z) c";
        let joined: String = linkify(t)
            .into_iter()
            .map(|s| match s {
                Segment::Text(s) | Segment::Url(s) => s,
            })
            .collect();
        assert_eq!(joined, t);
        // Multi-byte text around URLs.
        assert_eq!(urls("ünï http://ex.example/ä ü"), ["http://ex.example/ä"]);
        assert!(urls("").is_empty());
    }

    #[test]
    fn hrefs_are_checked() {
        let ok = |u: &str| safe_href(u, LinkType::Inline);
        assert_eq!(
            ok("https://example.com").as_deref(),
            Some("https://example.com")
        );
        assert_eq!(
            ok(" HTTP://example.com ").as_deref(),
            Some("HTTP://example.com")
        );
        assert_eq!(
            ok("mailto:a@example.com").as_deref(),
            Some("mailto:a@example.com")
        );
        assert_eq!(
            ok("https://example.com/a b").as_deref(),
            Some("https://example.com/a%20b")
        );
        for bad in [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            "java\tscript:alert(1)",
            "\u{1}javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "vbscript:x",
            "/relative/path",
            "relative",
            "#frag",
            "//example.com/protocol-relative",
            "http:/example.com",
            "https://",
            "mailto:",
            "file:///etc/passwd",
            "",
        ] {
            assert_eq!(ok(bad), None, "{bad:?}");
        }
        assert_eq!(
            safe_href("a@example.com", LinkType::Email).as_deref(),
            Some("mailto:a@example.com")
        );
    }

    #[test]
    fn raw_html_is_text() {
        let doc = parse("<script>alert(1)</script>\n\nhi <b>x</b>");
        assert_eq!(
            doc,
            vec![
                Node::El(
                    El::Para,
                    vec![Node::Text("<script>alert(1)</script>".into())]
                ),
                Node::El(El::Para, vec![Node::Text("hi <b>x</b>".into())]),
            ]
        );
    }

    #[test]
    fn unsafe_links_keep_only_text() {
        let doc = parse("[click](javascript:alert(1)) and ![pic](data:image/png;base64,AA)");
        assert_eq!(
            doc,
            vec![Node::El(El::Para, vec![Node::Text("click and pic".into())])]
        );
    }

    #[test]
    fn no_linkify_in_code_or_links() {
        let doc =
            parse("`http://a.example`\n\n```\nhttp://b.example\n```\n\n[t](http://c.example)");
        assert_eq!(
            doc,
            vec![
                Node::El(El::Para, vec![Node::Code("http://a.example".into())]),
                Node::CodeBlock("http://b.example\n".into()),
                Node::El(
                    El::Para,
                    vec![Node::Link {
                        href: "http://c.example".into(),
                        image: false,
                        children: vec![Node::Text("t".into())],
                    }]
                ),
            ]
        );
    }

    #[test]
    fn deep_nesting_is_capped() {
        let text = format!("{}deep", "> ".repeat(5000));
        let doc = parse(&text);
        fn depth(n: &[Node]) -> usize {
            n.iter()
                .map(|n| match n {
                    Node::El(_, c) | Node::Link { children: c, .. } => 1 + depth(c),
                    _ => 0,
                })
                .max()
                .unwrap_or(0)
        }
        assert!(depth(&doc) <= MAX_DEPTH);
    }

    #[test]
    fn table_alignment() {
        let doc = parse("| a | b |\n|:--|--:|\n| 1 | 2 |");
        let Node::El(El::Table, parts) = &doc[0] else {
            panic!("{doc:?}")
        };
        let Node::El(El::TableHead, head) = &parts[0] else {
            panic!("{parts:?}")
        };
        assert_eq!(
            head[1],
            Node::El(
                El::TableCell {
                    head: true,
                    align: Align::Right
                },
                vec![Node::Text("b".into())]
            )
        );
    }
}
