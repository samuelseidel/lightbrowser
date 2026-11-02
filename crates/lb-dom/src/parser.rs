//! Streaming HTML parser driver and fragment parsing.
//!
//! [`HtmlParser`] wraps html5ever's `Tokenizer` + `TreeBuilder`. Input is
//! pushed in chunks as it arrives from the network; [`HtmlParser::run`]
//! parses until it needs more input, reaches the end, or hits a `</script>`.
//! At a script it returns [`ParseStep::Script`]: the page layer runs the
//! script (which may call [`HtmlParser::write`] for `document.write`) and
//! then calls `run` again to resume.

use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{BufferQueue, Tokenizer, TokenizerOpts};
use html5ever::tree_builder::{TreeBuilder, TreeBuilderOpts};
use html5ever::{TokenizerResult, local_name, ns};

use crate::document::{Document, DomError};
use crate::node::{NodeId, NodeType};
use crate::sink::Sink;

/// What the parser did on a [`HtmlParser::run`] call.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ParseStep {
    /// All pushed input is consumed. Push more, or call `end`, then `run`.
    NeedInput,
    /// A script element was just closed. Run it (or decide not to), then
    /// call `run` to resume. Every `</script>` yields, whatever its `type`,
    /// `src`, `async` or `defer`; the page layer decides what to execute.
    /// Scripts inside `<template>` and SVG scripts yield too.
    Script(NodeId),
    /// End of input; the tree is complete.
    Done,
}

/// Parser options.
#[derive(Copy, Clone, Debug)]
pub struct ParserOptions {
    /// Affects `<noscript>` parsing. True for a normal page.
    pub scripting_enabled: bool,
}

impl Default for ParserOptions {
    fn default() -> Self {
        ParserOptions {
            scripting_enabled: true,
        }
    }
}

type Tok = Tokenizer<TreeBuilder<NodeId, Sink>>;

/// Incremental document parser. One per document load.
///
/// The parser does not own the document. Pass the same document to every
/// call; it is lent to html5ever for the duration of the call only.
pub struct HtmlParser {
    tok: Tok,
    /// Network input not yet parsed.
    input: BufferQueue,
    /// `document.write` text at the insertion point, parsed before `input`.
    inserted: BufferQueue,
    /// Script found while parsing written text; returned by the next `run`.
    pending_script: Option<NodeId>,
    /// Script we last returned; set until `run` is called again.
    blocked_on: Option<NodeId>,
    /// Incomplete UTF-8 sequence from the last byte chunk.
    utf8_tail: Vec<u8>,
    eof: bool,
    done: bool,
    encoding_hint: Option<String>,
}

impl Default for HtmlParser {
    fn default() -> Self {
        Self::new()
    }
}

impl HtmlParser {
    /// Parser for a fresh document (one created with [`Document::new`]).
    pub fn new() -> Self {
        Self::with_options(ParserOptions::default())
    }

    pub fn with_options(opts: ParserOptions) -> Self {
        let sink = Sink::new(Document::placeholder(), Document::DOCUMENT, true);
        let tb_opts = TreeBuilderOpts {
            scripting_enabled: opts.scripting_enabled,
            ..Default::default()
        };
        let tb = TreeBuilder::new(sink, tb_opts);
        HtmlParser {
            tok: Tokenizer::new(tb, TokenizerOpts::default()),
            input: BufferQueue::default(),
            inserted: BufferQueue::default(),
            pending_script: None,
            blocked_on: None,
            utf8_tail: Vec::new(),
            eof: false,
            done: false,
            encoding_hint: None,
        }
    }

    /// Queue decoded text. Does not parse; call [`HtmlParser::run`].
    pub fn push_str(&mut self, chunk: &str) {
        debug_assert!(!self.eof, "push after end");
        self.input.push_back(StrTendril::from_slice(chunk));
    }

    /// Queue UTF-8 bytes. Sequences split across chunks are joined; invalid
    /// bytes become U+FFFD. Other encodings must be decoded by the caller.
    pub fn push_bytes(&mut self, chunk: &[u8]) {
        let owned;
        let mut bytes = chunk;
        if !self.utf8_tail.is_empty() {
            let mut joined = std::mem::take(&mut self.utf8_tail);
            joined.extend_from_slice(chunk);
            owned = joined;
            bytes = &owned;
        }
        let mut out = String::with_capacity(bytes.len());
        loop {
            match std::str::from_utf8(bytes) {
                Ok(s) => {
                    out.push_str(s);
                    break;
                }
                Err(e) => {
                    let (valid, rest) = bytes.split_at(e.valid_up_to());
                    out.push_str(std::str::from_utf8(valid).unwrap_or_default());
                    match e.error_len() {
                        Some(n) => {
                            out.push('\u{FFFD}');
                            bytes = &rest[n..];
                        }
                        None => {
                            // Incomplete sequence at the end: keep for later.
                            self.utf8_tail.extend_from_slice(rest);
                            break;
                        }
                    }
                }
            }
        }
        if !out.is_empty() {
            self.input.push_back(StrTendril::from(out));
        }
    }

    /// Mark the end of input. Call `run` afterwards to finish the tree.
    pub fn end(&mut self) {
        if !self.utf8_tail.is_empty() {
            self.utf8_tail.clear();
            self.input.push_back(StrTendril::from_char('\u{FFFD}'));
        }
        self.eof = true;
    }

    /// Push a chunk and run: the common network-loop call.
    pub fn feed_str(&mut self, doc: &mut Document, chunk: &str) -> ParseStep {
        self.push_str(chunk);
        self.run(doc)
    }

    /// Push bytes and run.
    pub fn feed_bytes(&mut self, doc: &mut Document, chunk: &[u8]) -> ParseStep {
        self.push_bytes(chunk);
        self.run(doc)
    }

    /// Parse as far as possible. After a [`ParseStep::Script`], calling this
    /// again resumes after the script.
    pub fn run(&mut self, doc: &mut Document) -> ParseStep {
        if self.done {
            return ParseStep::Done;
        }
        self.blocked_on = None;
        if let Some(s) = self.pending_script.take() {
            self.blocked_on = Some(s);
            return ParseStep::Script(s);
        }
        self.lend(doc);
        let step = loop {
            let from_inserted = !self.inserted.is_empty();
            let queue = if from_inserted {
                &self.inserted
            } else {
                &self.input
            };
            match self.tok.feed(queue) {
                TokenizerResult::Script(s) => {
                    self.blocked_on = Some(s);
                    break ParseStep::Script(s);
                }
                TokenizerResult::EncodingIndicator(e) => {
                    self.encoding_hint.get_or_insert_with(|| e.to_string());
                }
                TokenizerResult::Done if from_inserted => {}
                TokenizerResult::Done => {
                    if self.eof {
                        self.tok.end();
                        self.done = true;
                        break ParseStep::Done;
                    }
                    break ParseStep::NeedInput;
                }
            }
        };
        self.take_back(doc);
        step
    }

    /// `document.write(text)` while a parser-blocking script runs: insert
    /// `text` at the insertion point and parse it right away, so the script
    /// sees the new nodes. A script in the written text is returned by the
    /// next `run`, before parsing continues.
    ///
    /// Returns false when there is no insertion point (the parser is not
    /// stopped at a script); the page layer then applies the
    /// `document.open` / ignore rules itself.
    pub fn write(&mut self, doc: &mut Document, text: &str) -> bool {
        if self.blocked_on.is_none() || self.done {
            return false;
        }
        self.inserted.push_back(StrTendril::from_slice(text));
        if self.pending_script.is_some() {
            return true;
        }
        self.lend(doc);
        loop {
            match self.tok.feed(&self.inserted) {
                TokenizerResult::Script(s) => {
                    self.pending_script = Some(s);
                    break;
                }
                TokenizerResult::EncodingIndicator(_) => {}
                TokenizerResult::Done => break,
            }
        }
        self.take_back(doc);
        true
    }

    /// Script the parser is currently stopped at, if any.
    pub fn blocked_on(&self) -> Option<NodeId> {
        self.blocked_on
    }

    /// True once `run` returned [`ParseStep::Done`].
    pub fn is_done(&self) -> bool {
        self.done
    }

    /// Source line where a parser-created `<script>` started (1-based).
    pub fn script_line(&self, script: NodeId) -> Option<u64> {
        self.sink().script_lines.borrow().get(&script).copied()
    }

    /// Number of parse errors seen so far.
    pub fn parse_error_count(&self) -> u32 {
        self.sink().errors.get()
    }

    /// Encoding named by the first `<meta charset>` / `http-equiv`, if any.
    /// The caller decides whether to re-decode; parsing just continues.
    pub fn encoding_hint(&self) -> Option<&str> {
        self.encoding_hint.as_deref()
    }

    fn sink(&self) -> &Sink {
        &self.tok.sink.sink
    }

    fn lend(&mut self, doc: &mut Document) {
        std::mem::swap(&mut *self.tok.sink.sink.doc.borrow_mut(), doc);
    }

    fn take_back(&mut self, doc: &mut Document) {
        std::mem::swap(&mut *self.tok.sink.sink.doc.borrow_mut(), doc);
    }
}

impl Document {
    /// HTML fragment parsing algorithm with `context` as the context
    /// element. Returns a new DocumentFragment holding the result. Scripts
    /// in it are marked already started. The mutation hook does not see the
    /// scratch tree.
    pub fn parse_fragment(&mut self, context: NodeId, html: &str) -> NodeId {
        let hook = self.set_mutation_hook(None);
        let root = self.create_document_fragment();
        let form = std::iter::once(context)
            .chain(self.ancestors(context))
            .find(|&n| self.is_html_element_named(n, &local_name!("form")));
        let quirks = self.quirks_mode();
        let doc = std::mem::replace(self, Document::placeholder());
        let sink = Sink::new(doc, root, false);
        let tb_opts = TreeBuilderOpts {
            quirks_mode: quirks,
            ..Default::default()
        };
        let tb = TreeBuilder::new_for_fragment(sink, context, form, tb_opts);
        let tok_opts = TokenizerOpts {
            initial_state: Some(tb.tokenizer_state_for_context_elem(true)),
            ..Default::default()
        };
        let tok = Tokenizer::new(tb, tok_opts);
        let input = BufferQueue::default();
        input.push_back(StrTendril::from_slice(html));
        while !matches!(tok.feed(&input), TokenizerResult::Done) {}
        tok.end();
        *self = tok.sink.sink.doc.into_inner();

        // The tree builder put an <html> root under `root`; lift its children.
        if let Some(html_root) = self.first_child(root) {
            self.detach(html_root);
            self.reparent_children(html_root, root);
        }
        self.set_mutation_hook(hook);
        root
    }

    /// `element.innerHTML = html`. For `<template>` this replaces the
    /// template contents.
    pub fn set_inner_html(&mut self, element: NodeId, html: &str) {
        let frag = self.parse_fragment(element, html);
        let target = self.template_contents(element).unwrap_or(element);
        self.remove_all_children(target);
        let kids: Vec<NodeId> = self.children(frag).collect();
        for k in kids {
            self.insert_unchecked(target, k, None);
        }
    }

    /// `element.outerHTML = html`. No-op for a parentless element.
    pub fn set_outer_html(&mut self, element: NodeId, html: &str) -> Result<(), DomError> {
        let Some(parent) = self.parent(element) else {
            return Ok(());
        };
        let context = match self.node_type(parent) {
            NodeType::Document => return Err(DomError::NoModificationAllowed),
            NodeType::DocumentFragment => self.create_element(
                html5ever::QualName::new(None, ns!(html), local_name!("body")),
                Vec::new(),
            ),
            _ => parent,
        };
        let frag = self.parse_fragment(context, html);
        let kids: Vec<NodeId> = self.children(frag).collect();
        for k in kids {
            self.insert_unchecked(parent, k, Some(element));
        }
        self.detach(element);
        Ok(())
    }
}
