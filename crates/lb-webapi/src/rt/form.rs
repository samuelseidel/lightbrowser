//! Form submission (HTML "submit a form"): the entry list and its three
//! encodings, byte for byte as Chrome 153 sends them (checked against
//! `profiles/headers/captures/chrome_153_linux_forms.jsonl`).
//!
//! `method="post"` becomes a POST navigation with the encoded body and the
//! action URL as is; anything else a GET with the urlencoded entries as the
//! query. The submitter's `formaction`/`formmethod`/`formenctype` win over
//! the form's attributes. Values are always UTF-8 (no `accept-charset` or
//! legacy document encodings); `target` is ignored (the form's own window
//! navigates); image buttons add no coordinates; file inputs send an empty
//! file.

use lb_dom::{Document, NodeFlags, NodeId, local_name};

use super::{FormBody, NavRequest, Window};

/// One entry of the form data set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Value {
    Text(String),
    /// A file input: no file chosen (the empty file, as Chrome sends it).
    File {
        filename: String,
    },
}

/// The form's encoding (`enctype`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Enctype {
    UrlEncoded,
    Multipart,
    TextPlain,
}

impl Enctype {
    fn parse(s: Option<&str>) -> Enctype {
        match s.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
            Some("multipart/form-data") => Enctype::Multipart,
            Some("text/plain") => Enctype::TextPlain,
            _ => Enctype::UrlEncoded,
        }
    }
}

/// Submit `form` (after its `submit` event, if any). `submitter` is the
/// button that submitted it; `user_activated` is set for trusted input.
pub(crate) fn submit_form(
    w: &Window,
    form: NodeId,
    submitter: Option<NodeId>,
    user_activated: bool,
) {
    let (action, method, enctype, entries) = w.with_doc(|d| {
        // A submitter's own attribute wins (only on submit buttons).
        let attr = |name: &str| {
            submitter
                .and_then(|s| d.get_attribute(s, &format!("form{name}")))
                .or_else(|| d.get_attribute(form, name))
                .map(str::to_owned)
        };
        let action = attr("action").filter(|a| !a.trim().is_empty());
        let method = attr("method").unwrap_or_default().to_ascii_lowercase();
        let enctype = Enctype::parse(attr("enctype").as_deref());
        (action, method, enctype, entry_list(w, d, form, submitter))
    });
    if method == "dialog" {
        return;
    }
    let url = match action {
        Some(a) => w.resolve_url(a.trim()),
        None => Some(w.url()),
    };
    let Some(mut url) = url else { return };
    let mut nav = NavRequest::get(url.clone(), false);
    nav.user_activated = user_activated;
    if method == "post" {
        let (content_type, data) = match enctype {
            Enctype::UrlEncoded => (
                "application/x-www-form-urlencoded".to_owned(),
                urlencoded(&entries).into_bytes(),
            ),
            Enctype::TextPlain => ("text/plain".to_owned(), text_plain(&entries).into_bytes()),
            Enctype::Multipart => {
                let b = boundary(w, form, entries.len());
                (
                    format!("multipart/form-data; boundary={b}"),
                    multipart(&entries, &b),
                )
            }
        };
        nav.post = Some(FormBody { content_type, data });
    } else {
        // GET: the entries replace the action's query (fragment stays).
        url.set_query(Some(&urlencoded(&entries)));
        nav.url = url;
    }
    w.request_navigation(nav);
}

/// The form's entries in tree order ("constructing the entry list").
fn entry_list(
    w: &Window,
    d: &Document,
    form: NodeId,
    submitter: Option<NodeId>,
) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for c in d.descendants(form) {
        // Form-associated custom elements submit what they set with
        // `ElementInternals.setFormValue` (ADR 0026).
        if d.flags(c).contains(NodeFlags::FORM_ASSOCIATED) {
            let entries = super::internals::submission_entries(w, d, c);
            out.extend(entries.into_iter().map(|(n, v)| (n, Value::Text(v))));
            continue;
        }
        let Some(name) = d.get_attribute(c, "name").filter(|n| !n.is_empty()) else {
            continue;
        };
        if disabled(d, c) {
            continue;
        }
        let local = d.local_name(c).map(|l| l.to_string()).unwrap_or_default();
        let stored = || w.values.borrow().get(&(0, c)).cloned();
        let value = match local.as_str() {
            "input" => {
                let t = super::dom::input_type(d, c);
                match t.as_str() {
                    "checkbox" | "radio" => {
                        if !super::dom::checked(d, c, "checked") {
                            continue;
                        }
                        Value::Text(attr_or(d, c, "value", "on"))
                    }
                    // Only the button that submitted the form.
                    "submit" => {
                        if submitter != Some(c) {
                            continue;
                        }
                        Value::Text(attr_or(d, c, "value", "Submit"))
                    }
                    "button" | "reset" | "image" => continue,
                    "file" => Value::File {
                        filename: String::new(),
                    },
                    "hidden" if name.eq_ignore_ascii_case("_charset_") => {
                        Value::Text("UTF-8".into())
                    }
                    _ => Value::Text(stored().unwrap_or_else(|| attr_or(d, c, "value", ""))),
                }
            }
            "button" => {
                let submit = d
                    .get_attribute(c, "type")
                    .is_none_or(|t| t.eq_ignore_ascii_case("submit"));
                if !submit || submitter != Some(c) {
                    continue;
                }
                Value::Text(attr_or(d, c, "value", ""))
            }
            "textarea" => {
                Value::Text(stored().unwrap_or_else(|| d.text_content(c).unwrap_or_default()))
            }
            "select" => {
                let opts: Vec<NodeId> = d
                    .descendants(c)
                    .filter(|&o| d.is_html_element_named(o, &local_name!("option")))
                    .collect();
                let selected: Vec<NodeId> = opts
                    .iter()
                    .copied()
                    .filter(|&o| super::dom::checked(d, o, "selected"))
                    .collect();
                let chosen = if d.has_attribute(c, "multiple") {
                    selected
                } else {
                    selected
                        .first()
                        .or(opts.first())
                        .copied()
                        .into_iter()
                        .collect()
                };
                for o in chosen {
                    let v = d
                        .get_attribute(o, "value")
                        .map(str::to_owned)
                        .unwrap_or_else(|| d.text_content(o).unwrap_or_default().trim().to_owned());
                    out.push((name.to_owned(), Value::Text(v)));
                }
                continue;
            }
            _ => continue,
        };
        out.push((name.to_owned(), value));
    }
    out
}

fn attr_or(d: &Document, n: NodeId, name: &str, default: &str) -> String {
    d.get_attribute(n, name).unwrap_or(default).to_owned()
}

/// `disabled` on the control or on a `<fieldset>` around it.
fn disabled(d: &Document, n: NodeId) -> bool {
    d.has_attribute(n, "disabled")
        || d.ancestors(n).any(|a| {
            d.is_html_element_named(a, &local_name!("fieldset")) && d.has_attribute(a, "disabled")
        })
}

/// Lone CR or LF become CRLF.
fn crlf(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\r' => {
                if it.peek() == Some(&'\n') {
                    it.next();
                }
                out.push_str("\r\n");
            }
            '\n' => out.push_str("\r\n"),
            c => out.push(c),
        }
    }
    out
}

fn text_of(v: &Value) -> &str {
    match v {
        Value::Text(s) => s,
        Value::File { filename } => filename,
    }
}

/// `application/x-www-form-urlencoded` (space as `+`, `*-._` kept).
pub(crate) fn urlencoded(entries: &[(String, Value)]) -> String {
    let mut s = url::form_urlencoded::Serializer::new(String::new());
    for (n, v) in entries {
        s.append_pair(&crlf(n), &crlf(text_of(v)));
    }
    s.finish()
}

/// `text/plain`: `name=value` lines, nothing escaped.
pub(crate) fn text_plain(entries: &[(String, Value)]) -> String {
    let mut out = String::new();
    for (n, v) in entries {
        out.push_str(&crlf(n));
        out.push('=');
        out.push_str(&crlf(text_of(v)));
        out.push_str("\r\n");
    }
    out
}

/// `multipart/form-data` with `boundary` (Blink's layout).
pub(crate) fn multipart(entries: &[(String, Value)], boundary: &str) -> Vec<u8> {
    // Names (and file names) are quoted with CR, LF and `"` escaped.
    let quote = |s: &str| {
        crlf(s)
            .replace('\r', "%0D")
            .replace('\n', "%0A")
            .replace('"', "%22")
    };
    let mut out = Vec::new();
    for (n, v) in entries {
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        out.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{}\"", quote(n)).as_bytes(),
        );
        match v {
            Value::Text(t) => {
                out.extend_from_slice(b"\r\n\r\n");
                out.extend_from_slice(crlf(t).as_bytes());
            }
            Value::File { filename } => {
                out.extend_from_slice(
                    format!(
                        "; filename=\"{}\"\r\nContent-Type: application/octet-stream\r\n\r\n",
                        quote(filename)
                    )
                    .as_bytes(),
                );
            }
        }
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    out
}

/// Blink's 64-entry table (`A`-`Z`, `a`-`z`, `0`-`9`, then `A`, `B` again).
const BOUNDARY_CHARS: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789AB";

/// `----WebKitFormBoundary` plus 16 characters. The stream comes from the
/// persona seed, the origin and the window clock: the same under virtual
/// time, different per submission in real time.
fn boundary(w: &Window, form: NodeId, n: usize) -> String {
    let what = format!("form-boundary:{:x}:{form:?}:{n}", w.clock.now().to_bits());
    let mut rng = super::rtc::rng(w, &what);
    let mut out = String::from("----WebKitFormBoundary");
    for _ in 0..4 {
        let r = rng.next_u32();
        for shift in [24, 16, 8, 0] {
            out.push(BOUNDARY_CHARS[((r >> shift) & 0x3f) as usize] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(n: &str, v: &str) -> (String, Value) {
        (n.into(), Value::Text(v.into()))
    }

    /// The captured form: `email`, `password`, a checkbox, a two-line
    /// textarea and the submit button.
    fn captured() -> Vec<(String, Value)> {
        vec![
            t("email", "a b@x.test"),
            t("password", "p&s=w%ö"),
            t("remember", "on"),
            t("note", "l1\nl2"),
            t("btn", "1"),
        ]
    }

    #[test]
    fn urlencoded_matches_chrome() {
        assert_eq!(
            urlencoded(&captured()),
            "email=a+b%40x.test&password=p%26s%3Dw%25%C3%B6&remember=on&note=l1%0D%0Al2&btn=1"
        );
    }

    #[test]
    fn text_plain_matches_chrome() {
        assert_eq!(
            text_plain(&captured()),
            "email=a b@x.test\r\npassword=p&s=w%ö\r\nremember=on\r\nnote=l1\r\nl2\r\nbtn=1\r\n"
        );
    }

    #[test]
    fn multipart_matches_chrome() {
        let b = "----WebKitFormBoundaryuZesKCUPvtRbZqFB";
        let body = String::from_utf8(multipart(&captured(), b)).unwrap();
        let part = |n: &str, v: &str| {
            format!("--{b}\r\nContent-Disposition: form-data; name=\"{n}\"\r\n\r\n{v}\r\n")
        };
        let want = [
            part("email", "a b@x.test"),
            part("password", "p&s=w%ö"),
            part("remember", "on"),
            part("note", "l1\r\nl2"),
            part("btn", "1"),
            format!("--{b}--\r\n"),
        ]
        .concat();
        assert_eq!(body, want);
    }

    #[test]
    fn multipart_quotes_names_and_sends_empty_files() {
        let e = vec![(
            "a\"b\nc".to_owned(),
            Value::File {
                filename: String::new(),
            },
        )];
        let body = String::from_utf8(multipart(&e, "X")).unwrap();
        assert_eq!(
            body,
            "--X\r\nContent-Disposition: form-data; name=\"a%22b%0D%0Ac\"; filename=\"\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n\r\n--X--\r\n"
        );
    }

    #[test]
    fn newlines_become_crlf() {
        assert_eq!(crlf("a\nb\rc\r\nd"), "a\r\nb\r\nc\r\nd");
    }

    #[test]
    fn enctype_parsing() {
        assert_eq!(Enctype::parse(None), Enctype::UrlEncoded);
        assert_eq!(
            Enctype::parse(Some(" Multipart/Form-Data")),
            Enctype::Multipart
        );
        assert_eq!(Enctype::parse(Some("text/plain")), Enctype::TextPlain);
        assert_eq!(Enctype::parse(Some("bogus")), Enctype::UrlEncoded);
    }
}
