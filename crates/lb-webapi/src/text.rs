//! Text extraction without layout (engine-neutral): `innerText`-like
//! text of an element, used by `HTMLElement.innerText` and
//! `lightbrowser fetch --dump text`.

use lb_dom::{Document, NodeId, local_name};

/// `innerText` approximation without layout: skips hidden-by-default
/// elements, turns `<br>` and block boundaries into newlines.
pub fn inner_text(d: &Document, root: NodeId) -> String {
    fn block(local: &str) -> bool {
        matches!(
            local,
            "p" | "div"
                | "li"
                | "ul"
                | "ol"
                | "dl"
                | "dt"
                | "dd"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "tr"
                | "table"
                | "section"
                | "article"
                | "header"
                | "footer"
                | "nav"
                | "aside"
                | "main"
                | "form"
                | "fieldset"
                | "pre"
                | "blockquote"
                | "figure"
                | "figcaption"
                | "address"
                | "hr"
                | "title"
                | "details"
                | "summary"
                | "caption"
                | "thead"
                | "tbody"
                | "tfoot"
                | "body"
                | "html"
                | "option"
        )
    }
    fn skip(local: &str) -> bool {
        matches!(
            local,
            "script" | "style" | "noscript" | "template" | "head" | "iframe" | "object" | "select"
        )
    }
    fn walk(d: &Document, n: NodeId, out: &mut String) {
        for c in d.children(n) {
            match d.node_type(c) {
                lb_dom::NodeType::Text => {
                    let t = d.character_data(c).unwrap_or("");
                    let pre = d
                        .ancestors(c)
                        .any(|a| d.is_html_element_named(a, &local_name!("pre")));
                    if pre {
                        out.push_str(t);
                    } else {
                        let mut last_space = out.ends_with([' ', '\n']) || out.is_empty();
                        for ch in t.chars() {
                            if ch.is_ascii_whitespace() {
                                if !last_space {
                                    out.push(' ');
                                    last_space = true;
                                }
                            } else {
                                out.push(ch);
                                last_space = false;
                            }
                        }
                    }
                }
                lb_dom::NodeType::Element => {
                    let local = d.local_name(c).map(|l| l.to_string()).unwrap_or_default();
                    if skip(&local) || d.has_attribute(c, "hidden") {
                        continue;
                    }
                    if local == "br" {
                        trim_trailing_space(out);
                        out.push('\n');
                        continue;
                    }
                    let is_block = block(&local);
                    if is_block && !out.is_empty() && !out.ends_with('\n') {
                        trim_trailing_space(out);
                        out.push('\n');
                    }
                    walk(d, c, out);
                    if is_block && !out.ends_with('\n') {
                        trim_trailing_space(out);
                        out.push('\n');
                    }
                    if matches!(local.as_str(), "td" | "th") {
                        out.push('\t');
                    }
                }
                _ => {}
            }
        }
    }
    fn trim_trailing_space(s: &mut String) {
        while s.ends_with(' ') {
            s.pop();
        }
    }
    let mut out = String::new();
    walk(d, root, &mut out);
    let lines: Vec<&str> = out.lines().map(str::trim).collect();
    let mut joined = lines.join("\n");
    while joined.contains("\n\n\n") {
        joined = joined.replace("\n\n\n", "\n\n");
    }
    joined.trim_matches('\n').to_owned()
}

/// Text of a whole document: its body (or root element).
pub fn document_text(d: &Document) -> String {
    match d.body().or_else(|| d.document_element()) {
        Some(b) => inner_text(d, b),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_of_blocks() {
        let d = Document::parse_html(
            "<title>T</title><style>p{}</style><body><p>Hello  <b>world</b></p><script>x()</script><div>a<br>b</div><ul><li>1</li><li>2</li></ul></body>",
        );
        assert_eq!(document_text(&d), "Hello world\na\nb\n1\n2");
    }
}
