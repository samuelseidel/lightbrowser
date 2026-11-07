# ADR 0042: Content extraction as `fetch --dump markdown | links | json`

Status: Accepted (2026-11-07).

## Context

`lightbrowser fetch` could print the DOM after scripts ran (`--dump html`)
and its `innerText`-like text (`--dump text`). Crawl and scraping tools
(Lightpanda's markdown dump, the extract commands of scraping CLIs) also
give the page as Markdown, as a link list, and as a small JSON summary,
which is what a test that checks "what did the page render" wants to
read. This is neutral tooling for our own loopback test pages: it reads
the document and style `fetch` already built and sends nothing.

## Decision

Three new `--dump` values, made by a library module, `lb_page::extract`
(`crates/lb-page/src/extract.rs`; no new crate, no new dependency). The
CLI change is additive: `Dump` gets three variants, `Loaded` an
`Option<Extracted>`, and `load_page` fills it only for these modes. The
output of `--dump html` and `--dump text` is unchanged. An unknown value
still fails with exit code 2 and now lists all five: `--dump must be one
of html, text, markdown, links, json, got "x"`.

### What is read

- The post-script DOM of the main document, walked in the **flat tree**
  (`Document::flat_children`): shadow content appears where the host is,
  slotted nodes inside their slot, unassigned light children not at all.
- Hidden content uses the page's own layout engine in the chrome build
  (`Page::extract`, `Window::with_layout_engine`): `display: none` skips a
  subtree, `visibility` other than `visible` skips the text of that
  element only (a visible child still shows). The `hidden` attribute,
  closed `dialog`s, the body of a closed `details` (everything but its
  first `summary`) and an inline `content-visibility: hidden` are skipped
  by name or attribute, in both builds (a style sheet's
  `content-visibility` is not seen: the style engine does not know the
  property). `display: none` on the root element hides the page. The firefox build, which has no
  style engine in `lb-page` yet, uses `attribute_probe` (the `hidden`
  attribute and inline `style` only). The converter takes the style
  answers as a closure, so it is engine-neutral.
- The base URL is the first `<base href>` that resolves against the
  document URL (`window.url()` after scripts), else the document URL.

### Markdown

- Skipped: `script style template noscript head iframe frame object embed
  canvas video audio select datalist textarea input option meta link
  title`, non-HTML elements (SVG, MathML) and the hidden content above.
  An `iframe` has no browsing context content here, so it adds nothing.
- Blocks: h1-h6 (`#`), paragraphs, ordered and unordered lists, `pre`,
  `blockquote`, `hr` (`---`), tables. Containers (`div`, `section`,
  `article`, `dl`, `details`, `figure`, ...) only separate paragraphs.
  An inline element that holds blocks (a custom element, a `span` around
  a `div`) is transparent: its blocks are blocks.
- Lists: `-` bullets, `1.` numbers starting at `start`, `li value` resets
  the count, hidden items are not counted; continuation lines are indented
  by the marker's width (`10. ` is 4); nested lists follow their item's
  text tightly (a numbered list that does not start at 1 cannot interrupt
  a paragraph in CommonMark, so a blank line separates it, which does not
  make the outer list loose), a list is loose (blank lines between items)
  when an item has two blocks apart from nested lists. A list right after
  another list uses the other marker than that list did (`-` and `*`,
  `.` and `)` alternate), so neighbours never merge, however many there
  are. The first block of a bullet item that is an `hr` is written `***`
  (`- ---` would be a thematic break). Numbers stay within nine digits
  (CommonMark's limit; a reader only uses the first number of a list).
  Block quotes and list items nest with their `>` and indentation to 24
  levels; deeper ones are written as plain blocks, because every level
  re-prefixes every line below it and the output would grow with depth
  times lines (390 nested quotes gave 78 MB from 410 KB). An item with nothing to show (hidden, icon only, an empty nested
  list) is not an item and not counted; a list with no items is dropped.
- Inline: links `[text](absolute URL)` (an `href` that does not resolve or
  is `javascript:` leaves the text without a link; a link with empty
  text is dropped; `(`, `)` and `\` in the URL are escaped), images
  `![alt](absolute src)` (only `src`, no `srcset`; an image without
  `src` adds nothing), `em`/`i` as `*`, `strong`/`b` as `**`,
  `del`/`s`/`strike` as `~~`, `code`/`kbd`/`samp`/`tt` as backtick spans
  with a fence longer than any backtick run inside. Edge whitespace of an
  emphasis, link or code span goes outside its markers. A closing `*`,
  `**` or `~~` that follows punctuation and precedes a letter or digit
  would not close in CommonMark, so a space is written after it.
  A span inside a span of its own kind (`<b><b>`) is written once; two
  spans of one kind with nothing between them (`<b>a</b><b>b</b>`) are
  one span. Spans that use the same marker character but are of another
  kind (`<b>a</b><i>b</i>`, which would give `**a***b*`) and two code
  spans in a row are separated by U+200B, or by a space when the second
  span starts with punctuation or the first ends after punctuation
  (neither would flank otherwise). A literal `!` before a link is
  escaped (`\![x](u)` would be an image). An inline span that holds
  blocks does not carry its markers into them. A link that holds blocks
  stays one link: its blocks are joined with spaces; headings inside it,
  or inside a table cell, are still listed as headings (and count for the
  `h1` rule). The control characters U+0001 to U+0004, which the
  converter uses as buffer markers, are removed from page text first,
  together with the other control characters except HTML whitespace
  (VT, FS, NEL, DEL, ...: a line end to some readers); U+2028 and U+2029
  become spaces. In a `pre`, a lone CR (from `&#13;`) becomes a newline
  and a form feed is dropped, so page text cannot end a block quote or a
  list item.
- `<br>` is a backslash hard break (never two trailing spaces); two in a
  row, with only whitespace between, end the paragraph.
- Code blocks are fenced with at least three backticks, more when the
  text holds a longer run; the info string is the `language-x` class of
  the first child `code` or of the `pre`, if it has only
  `[A-Za-z0-9_+#.-]`. Their text is kept as is (a `br` is a newline).
- Tables are GFM. The header is the `thead` row, or a first row made only
  of `th`; otherwise an empty header row is written (GFM needs one).
  Rows are `thead`, `tbody`/direct rows, `tfoot`, padded to the widest
  row. `colspan` adds empty cells, at most 100 per cell (`rowspan` is not
  modelled). When padding to a rectangle would cost more than eight times
  the cells there are (plus 4096), the table is written as one `| a | b |`
  line per row without padding, so the output stays linear in the input. Alignment
  is the first `align` attribute or inline `text-align` found in a column,
  header first. `|` becomes `\|`; block content and `<br>` in a cell
  become spaces. A `caption` is a paragraph before the table.
- Whitespace collapses as rendering collapses it; NBSP is kept; a block
  holding only whitespace or NBSP is dropped. Markdown specials are
  escaped as text: `\ ` * [ ] <` always, `_` unless between two letters or
  digits, `~` always (a lone `~` is strikethrough in GFM, three at the start of a line open a fence), `&` when it would read as a character
  reference, and at the start of a line `#`, `-`, `+`, `=`, `>` and
  `1.` / `1)` where they would start a block (not in cells and
  headings). A trailing run of `#` after a space in a heading or the title
  is escaped (it would be a closing sequence). Trailing spaces and tabs
  are removed from every line, including inside code blocks.
- Blocks are joined by one blank line and the text ends with one newline;
  an empty page gives an empty string. The text starts with `# <title>`
  only when no non-empty, visible `h1` was written.
- Element nesting deeper than 400 levels is not followed further: the
  rest of that subtree is kept as one paragraph of plain text (stack
  safety; the text is not lost).

### Links

One line per `a` with an `href`, in flat-tree document order:
`URL<TAB>text`. The URL is absolute; the text is the whitespace-normalised
text of the anchor (images give their `alt`, blocks and `br` give spaces;
control characters are dropped and U+2028/U+2029 are spaces, so a line is
one anchor).
`href` values that start with `#` (after trimming), `javascript:` values
and values that do not resolve are dropped; entries with the same URL and
text are dropped after the first. `a` elements inside `script`, `style`,
`template`, `noscript` and non-HTML (SVG) content are not listed. Anchors the
Markdown leaves out (hidden by attribute or `display: none`, inside a
hidden subtree or a closed `dialog`, or `visibility: hidden` themselves)
are not listed, so the three outputs agree about what is shown.

### JSON

One object, keys in this order, two-space indent, trailing newline, written
by hand so the order does not depend on a `serde_json` feature:
`url` (as requested, normalised), `final_url`, `status`, `title`
(`document.title`), `lang` (root `lang`, `null` when absent or empty),
`description` (first `meta name=description` with a non-empty
`content`, normalised, or `null`), `canonical` (`link rel~=canonical` `href`, absolute, or `null`),
`headings` (`[{level, text}]`, in document order, including headings
inside links and table cells; a heading with no text, such as one that
holds only an image without `alt`, is written in the Markdown but not
listed),
`links` (`[{url, text, rel}]`, the same list as the links dump with the
normalised `rel`), `text` (the `--dump text` string) and `markdown`.

## Tests

The expectations are hand-written from these rules, not browser
recordings.

- `crates/lb-page/tests/extract/`: 20 cases (`NAME.html` or `NAME.txt`
  with `NAME.md`, `NAME.links`, `NAME.json`; `gen_json.py` writes the JSON
  files from hand-written metadata). `tests/extract.rs` loads each page
  from a loopback server and compares all three outputs. Cases: headings,
  nested and adjacent lists, relative links with `<base>`, images,
  emphasis and code spans, code blocks, quotes and rules, tables,
  hidden content (attribute, inline style, style sheet class, visibility),
  whitespace and entities, escapes, skipped elements and an iframe, shadow
  DOM (declarative and script-attached, with slots), blocks inside links,
  a page without `h1`, a page built by script, a `text/plain` response,
  and `regress` (round 2: adjacent lists, empty items, nested `ol start`,
  marker characters, a heading inside a link, emphasis next to
  punctuation, an empty first description) and `regress3` (round 3: a
  link after `!`, `~` at the start of a span, CR in `pre`, an `hr` item,
  adjacent spans, a trailing `#`, a nine-digit number, a heading in a
  table cell, a closed `details`, `content-visibility`).
- 35 unit tests in the module (escaping, base selection, link filtering,
  numbering, emphasis edges, breaks, tables, fences, probes, depth limit,
  JSON layout, and one per round 2 problem: adjacent lists, empty items,
  nested `ol start`, a table of `colspan` cells, marker characters,
  headings in links, emphasis next to punctuation, hidden links, an empty
  description; round 3 adds one per problem: a link after `!`, tildes,
  control characters, an `hr` item, adjacent spans, a trailing `#`, list
  numbers, headings in cells, closed `details`, one walk per nested chain,
  the depth limit for quotes and lists).
- `bin/lightbrowser/tests/extract_cli.rs`: the built binary against a
  loopback `python3 -m http.server` that serves the same files; all
  three modes for eight cases equal the golden files, `json.text` equals
  `--dump text`, `--dump html` is unchanged, an error status shows in the
  JSON (exit code 1), and a bad `--dump` value exits with 2 and lists the
  five values.

## Consequences

- No new dependency, no change to the Persona, network or page-visible
  surface: extraction runs after the load, on the page's worker, and never
  mutates the document. It does create the page's layout engine (as
  `getComputedStyle` would), so one more engine's memory is allocated for
  these three modes only.
- The links dump leaves out hidden anchors (round 2; the first version
  listed them). A tool that wants every anchor of the DOM should read
  `--dump html`.
- Round 3 checks: a differential fuzz of random inline and block pages
  against `markdown_it` (commonmark with tables and strikethrough; word
  order, per-word span kinds, counts of items, links, code blocks, quotes,
  tables, rules and heading levels) found no difference in 750 block
  pages and, bar one, in 750 inline pages of the final code: an intraword `**` span
  inside an `*` span after a literal `***`, where `markdown_it` and the
  CommonMark rule of three disagree. A 1.25 MB chain of 390 nested spans
  with a block at the bottom costs about 4 s more than `--dump text` in a
  debug build (8.3 s against 4.4 s; it was 16 s against 4 s), the same as
  a page without nesting, so no walk repeats per level.
- Not done: `rowspan`, `srcset`/`picture`, reversed lists, task-list
  checkboxes, definition lists as such, `<q>` quotes, math, text of
  `iframe`s and of `svg`. An inline element is inline or block by its tag,
  not by its computed `display` (`<span style="display:block">` stays
  inline). `href=""` is dropped from the link list (it names the page
  itself) and fragment-only links are dropped from the list but kept in
  the Markdown. The JSON `text` is the `--dump text` string, so it keeps
  what the Markdown leaves out (a closed `details`, `display: none`
  text) and not shadow content. Text inside `pre` loses trailing spaces per
  line. Hidden-ness in the firefox build ignores style sheets.
