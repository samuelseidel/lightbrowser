//! Element → interface mapping (Chrome's HTMLElement subclasses) and
//! reflected content attributes.

use lb_dom::{Document, NodeId, NodeType, ns};

/// Interface name of a node's wrapper.
pub(crate) fn node_interface(doc: &Document, id: NodeId, _slot: u32) -> &'static str {
    match doc.node_type(id) {
        NodeType::Document => {
            if doc.is_html() {
                "HTMLDocument"
            } else {
                "XMLDocument"
            }
        }
        NodeType::Text => "Text",
        NodeType::Comment => "Comment",
        NodeType::DocumentFragment => {
            if doc.is_shadow_root(id) {
                "ShadowRoot"
            } else {
                "DocumentFragment"
            }
        }
        NodeType::DocumentType => "DocumentType",
        NodeType::ProcessingInstruction => "ProcessingInstruction",
        NodeType::Element => {
            let Some(name) = doc.qual_name(id) else {
                return "Element";
            };
            if name.ns == ns!(html) {
                html_interface(&name.local)
            } else if name.ns == ns!(svg) {
                match &*name.local {
                    "svg" => "SVGSVGElement",
                    "g" => "SVGGElement",
                    "path" => "SVGPathElement",
                    "rect" => "SVGRectElement",
                    "circle" => "SVGCircleElement",
                    "text" => "SVGTextElement",
                    "script" => "SVGScriptElement",
                    "style" => "SVGStyleElement",
                    "title" => "SVGTitleElement",
                    _ => "SVGElement",
                }
            } else if name.ns == ns!(mathml) {
                "MathMLElement"
            } else {
                "Element"
            }
        }
    }
}

/// HTML element interface by local name (HTML spec element index, as in
/// Chrome 153).
pub(crate) fn html_interface(local: &str) -> &'static str {
    match local {
        "a" => "HTMLAnchorElement",
        "area" => "HTMLAreaElement",
        "audio" => "HTMLAudioElement",
        "br" => "HTMLBRElement",
        "base" => "HTMLBaseElement",
        "body" => "HTMLBodyElement",
        "button" => "HTMLButtonElement",
        "canvas" => "HTMLCanvasElement",
        "dl" => "HTMLDListElement",
        "data" => "HTMLDataElement",
        "datalist" => "HTMLDataListElement",
        "details" => "HTMLDetailsElement",
        "dialog" => "HTMLDialogElement",
        "dir" => "HTMLDirectoryElement",
        "div" => "HTMLDivElement",
        "embed" => "HTMLEmbedElement",
        "fieldset" => "HTMLFieldSetElement",
        "font" => "HTMLFontElement",
        "form" => "HTMLFormElement",
        "frame" => "HTMLFrameElement",
        "frameset" => "HTMLFrameSetElement",
        "hr" => "HTMLHRElement",
        "head" => "HTMLHeadElement",
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => "HTMLHeadingElement",
        "html" => "HTMLHtmlElement",
        "iframe" => "HTMLIFrameElement",
        "img" => "HTMLImageElement",
        "input" => "HTMLInputElement",
        "li" => "HTMLLIElement",
        "label" => "HTMLLabelElement",
        "legend" => "HTMLLegendElement",
        "link" => "HTMLLinkElement",
        "map" => "HTMLMapElement",
        "marquee" => "HTMLMarqueeElement",
        "menu" => "HTMLMenuElement",
        "meta" => "HTMLMetaElement",
        "meter" => "HTMLMeterElement",
        "ins" | "del" => "HTMLModElement",
        "ol" => "HTMLOListElement",
        "object" => "HTMLObjectElement",
        "optgroup" => "HTMLOptGroupElement",
        "option" => "HTMLOptionElement",
        "output" => "HTMLOutputElement",
        "p" => "HTMLParagraphElement",
        "param" => "HTMLParamElement",
        "picture" => "HTMLPictureElement",
        "pre" | "listing" | "xmp" => "HTMLPreElement",
        "progress" => "HTMLProgressElement",
        "q" | "blockquote" => "HTMLQuoteElement",
        "script" => "HTMLScriptElement",
        "select" => "HTMLSelectElement",
        "slot" => "HTMLSlotElement",
        "source" => "HTMLSourceElement",
        "span" => "HTMLSpanElement",
        "style" => "HTMLStyleElement",
        "caption" => "HTMLTableCaptionElement",
        "td" | "th" => "HTMLTableCellElement",
        "col" | "colgroup" => "HTMLTableColElement",
        "table" => "HTMLTableElement",
        "tr" => "HTMLTableRowElement",
        "thead" | "tbody" | "tfoot" => "HTMLTableSectionElement",
        "template" => "HTMLTemplateElement",
        "textarea" => "HTMLTextAreaElement",
        "time" => "HTMLTimeElement",
        "title" => "HTMLTitleElement",
        "track" => "HTMLTrackElement",
        "ul" => "HTMLUListElement",
        "video" => "HTMLVideoElement",
        // Elements with no own interface (HTML spec "HTMLElement" list).
        "abbr" | "address" | "article" | "aside" | "b" | "bdi" | "bdo" | "cite" | "code" | "dd"
        | "dfn" | "dt" | "em" | "figcaption" | "figure" | "footer" | "header" | "hgroup" | "i"
        | "kbd" | "main" | "mark" | "nav" | "noscript" | "rp" | "rt" | "ruby" | "s" | "samp"
        | "search" | "section" | "small" | "strong" | "sub" | "summary" | "sup" | "u" | "var"
        | "wbr" | "acronym" | "basefont" | "big" | "center" | "nobr" | "noembed" | "noframes"
        | "plaintext" | "rb" | "rtc" | "strike" | "tt" => "HTMLElement",
        // Custom elements (valid names contain a hyphen) are HTMLElement.
        l if l.contains('-') => "HTMLElement",
        _ => "HTMLUnknownElement",
    }
}

/// How a reflected IDL attribute maps to its content attribute.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Reflect {
    /// DOMString, missing = "".
    Str,
    /// Boolean: presence.
    Bool,
    /// USVString URL resolved against the base URL.
    Url,
    /// `long` with a default.
    Long(i32),
    /// `unsigned long` with a default.
    ULong(u32),
    /// Enumerated attribute: limited to known values, else the default
    /// (`missing default`, `invalid default`, values...).
    Enum(&'static str, &'static [&'static str]),
}

/// `(interface, IDL name, content attribute, kind)`.
pub(crate) const REFLECTED: &[(&str, &str, &str, Reflect)] = &[
    ("Element", "id", "id", Reflect::Str),
    ("Element", "className", "class", Reflect::Str),
    ("Element", "slot", "slot", Reflect::Str),
    ("HTMLElement", "title", "title", Reflect::Str),
    ("HTMLElement", "lang", "lang", Reflect::Str),
    ("HTMLElement", "accessKey", "accesskey", Reflect::Str),
    ("HTMLElement", "hidden", "hidden", Reflect::Bool),
    ("HTMLElement", "inert", "inert", Reflect::Bool),
    ("HTMLElement", "autofocus", "autofocus", Reflect::Bool),
    ("HTMLElement", "nonce", "nonce", Reflect::Str),
    (
        "HTMLElement",
        "dir",
        "dir",
        Reflect::Enum("", &["ltr", "rtl", "auto"]),
    ),
    ("HTMLAnchorElement", "href", "href", Reflect::Url),
    ("HTMLAnchorElement", "target", "target", Reflect::Str),
    ("HTMLAnchorElement", "download", "download", Reflect::Str),
    ("HTMLAnchorElement", "rel", "rel", Reflect::Str),
    ("HTMLAnchorElement", "hreflang", "hreflang", Reflect::Str),
    ("HTMLAnchorElement", "type", "type", Reflect::Str),
    ("HTMLAnchorElement", "name", "name", Reflect::Str),
    ("HTMLAnchorElement", "ping", "ping", Reflect::Str),
    ("HTMLAreaElement", "href", "href", Reflect::Url),
    ("HTMLAreaElement", "alt", "alt", Reflect::Str),
    ("HTMLBaseElement", "target", "target", Reflect::Str),
    ("HTMLImageElement", "src", "src", Reflect::Url),
    ("HTMLImageElement", "alt", "alt", Reflect::Str),
    ("HTMLImageElement", "srcset", "srcset", Reflect::Str),
    ("HTMLImageElement", "sizes", "sizes", Reflect::Str),
    ("HTMLImageElement", "useMap", "usemap", Reflect::Str),
    ("HTMLImageElement", "isMap", "ismap", Reflect::Bool),
    ("HTMLImageElement", "name", "name", Reflect::Str),
    ("HTMLImageElement", "width", "width", Reflect::ULong(0)),
    ("HTMLImageElement", "height", "height", Reflect::ULong(0)),
    ("HTMLScriptElement", "src", "src", Reflect::Url),
    ("HTMLScriptElement", "type", "type", Reflect::Str),
    ("HTMLScriptElement", "noModule", "nomodule", Reflect::Bool),
    ("HTMLScriptElement", "charset", "charset", Reflect::Str),
    ("HTMLScriptElement", "defer", "defer", Reflect::Bool),
    ("HTMLScriptElement", "integrity", "integrity", Reflect::Str),
    ("HTMLScriptElement", "event", "event", Reflect::Str),
    ("HTMLScriptElement", "htmlFor", "for", Reflect::Str),
    ("HTMLLinkElement", "href", "href", Reflect::Url),
    ("HTMLLinkElement", "rel", "rel", Reflect::Str),
    ("HTMLLinkElement", "media", "media", Reflect::Str),
    ("HTMLLinkElement", "type", "type", Reflect::Str),
    ("HTMLLinkElement", "hreflang", "hreflang", Reflect::Str),
    ("HTMLLinkElement", "as", "as", Reflect::Str),
    ("HTMLLinkElement", "disabled", "disabled", Reflect::Bool),
    ("HTMLStyleElement", "media", "media", Reflect::Str),
    ("HTMLStyleElement", "disabled", "disabled", Reflect::Bool),
    ("HTMLMetaElement", "name", "name", Reflect::Str),
    ("HTMLMetaElement", "content", "content", Reflect::Str),
    ("HTMLMetaElement", "httpEquiv", "http-equiv", Reflect::Str),
    ("HTMLMetaElement", "media", "media", Reflect::Str),
    ("HTMLFormElement", "name", "name", Reflect::Str),
    ("HTMLFormElement", "target", "target", Reflect::Str),
    ("HTMLFormElement", "noValidate", "novalidate", Reflect::Bool),
    (
        "HTMLFormElement",
        "acceptCharset",
        "accept-charset",
        Reflect::Str,
    ),
    (
        "HTMLFormElement",
        "method",
        "method",
        Reflect::Enum("get", &["get", "post", "dialog"]),
    ),
    ("HTMLInputElement", "name", "name", Reflect::Str),
    (
        "HTMLInputElement",
        "placeholder",
        "placeholder",
        Reflect::Str,
    ),
    ("HTMLInputElement", "disabled", "disabled", Reflect::Bool),
    ("HTMLInputElement", "required", "required", Reflect::Bool),
    ("HTMLInputElement", "readOnly", "readonly", Reflect::Bool),
    ("HTMLInputElement", "multiple", "multiple", Reflect::Bool),
    (
        "HTMLInputElement",
        "autocomplete",
        "autocomplete",
        Reflect::Str,
    ),
    ("HTMLInputElement", "min", "min", Reflect::Str),
    ("HTMLInputElement", "max", "max", Reflect::Str),
    ("HTMLInputElement", "step", "step", Reflect::Str),
    ("HTMLInputElement", "pattern", "pattern", Reflect::Str),
    ("HTMLInputElement", "accept", "accept", Reflect::Str),
    ("HTMLInputElement", "alt", "alt", Reflect::Str),
    ("HTMLInputElement", "src", "src", Reflect::Url),
    ("HTMLInputElement", "defaultValue", "value", Reflect::Str),
    (
        "HTMLInputElement",
        "defaultChecked",
        "checked",
        Reflect::Bool,
    ),
    ("HTMLButtonElement", "name", "name", Reflect::Str),
    ("HTMLButtonElement", "value", "value", Reflect::Str),
    ("HTMLButtonElement", "disabled", "disabled", Reflect::Bool),
    (
        "HTMLButtonElement",
        "type",
        "type",
        Reflect::Enum("submit", &["submit", "reset", "button"]),
    ),
    ("HTMLSelectElement", "name", "name", Reflect::Str),
    ("HTMLSelectElement", "disabled", "disabled", Reflect::Bool),
    ("HTMLSelectElement", "multiple", "multiple", Reflect::Bool),
    ("HTMLSelectElement", "required", "required", Reflect::Bool),
    ("HTMLTextAreaElement", "name", "name", Reflect::Str),
    (
        "HTMLTextAreaElement",
        "placeholder",
        "placeholder",
        Reflect::Str,
    ),
    ("HTMLTextAreaElement", "disabled", "disabled", Reflect::Bool),
    ("HTMLTextAreaElement", "readOnly", "readonly", Reflect::Bool),
    ("HTMLTextAreaElement", "required", "required", Reflect::Bool),
    ("HTMLOptionElement", "disabled", "disabled", Reflect::Bool),
    ("HTMLOptionElement", "label", "label", Reflect::Str),
    (
        "HTMLOptionElement",
        "defaultSelected",
        "selected",
        Reflect::Bool,
    ),
    ("HTMLLabelElement", "htmlFor", "for", Reflect::Str),
    ("HTMLIFrameElement", "src", "src", Reflect::Url),
    ("HTMLIFrameElement", "srcdoc", "srcdoc", Reflect::Str),
    ("HTMLIFrameElement", "name", "name", Reflect::Str),
    ("HTMLIFrameElement", "width", "width", Reflect::Str),
    ("HTMLIFrameElement", "height", "height", Reflect::Str),
    ("HTMLIFrameElement", "allow", "allow", Reflect::Str),
    ("HTMLCanvasElement", "width", "width", Reflect::ULong(300)),
    ("HTMLCanvasElement", "height", "height", Reflect::ULong(150)),
    ("HTMLMediaElement", "src", "src", Reflect::Url),
    ("HTMLMediaElement", "autoplay", "autoplay", Reflect::Bool),
    ("HTMLMediaElement", "loop", "loop", Reflect::Bool),
    ("HTMLMediaElement", "controls", "controls", Reflect::Bool),
    ("HTMLSourceElement", "src", "src", Reflect::Url),
    ("HTMLSourceElement", "type", "type", Reflect::Str),
    (
        "HTMLTableCellElement",
        "colSpan",
        "colspan",
        Reflect::ULong(1),
    ),
    (
        "HTMLTableCellElement",
        "rowSpan",
        "rowspan",
        Reflect::ULong(1),
    ),
    // Obsolete table presentation attributes (`bgColor` is left out: it
    // maps null to "", which `Reflect::Str` does not).
    ("HTMLTableElement", "align", "align", Reflect::Str),
    ("HTMLTableElement", "border", "border", Reflect::Str),
    ("HTMLTableElement", "frame", "frame", Reflect::Str),
    ("HTMLTableElement", "rules", "rules", Reflect::Str),
    ("HTMLTableElement", "summary", "summary", Reflect::Str),
    ("HTMLTableElement", "width", "width", Reflect::Str),
    (
        "HTMLTableElement",
        "cellPadding",
        "cellpadding",
        Reflect::Str,
    ),
    (
        "HTMLTableElement",
        "cellSpacing",
        "cellspacing",
        Reflect::Str,
    ),
    ("HTMLTableSectionElement", "align", "align", Reflect::Str),
    ("HTMLTableSectionElement", "ch", "char", Reflect::Str),
    ("HTMLTableSectionElement", "chOff", "charoff", Reflect::Str),
    ("HTMLTableSectionElement", "vAlign", "valign", Reflect::Str),
    ("HTMLTableRowElement", "align", "align", Reflect::Str),
    ("HTMLTableRowElement", "ch", "char", Reflect::Str),
    ("HTMLTableRowElement", "chOff", "charoff", Reflect::Str),
    ("HTMLTableRowElement", "vAlign", "valign", Reflect::Str),
    ("HTMLOListElement", "reversed", "reversed", Reflect::Bool),
    ("HTMLOListElement", "start", "start", Reflect::Long(1)),
    ("HTMLOListElement", "type", "type", Reflect::Str),
    ("HTMLLIElement", "value", "value", Reflect::Long(0)),
    ("HTMLDetailsElement", "open", "open", Reflect::Bool),
    ("HTMLDialogElement", "open", "open", Reflect::Bool),
    ("HTMLTimeElement", "dateTime", "datetime", Reflect::Str),
    ("HTMLDataElement", "value", "value", Reflect::Str),
    ("HTMLQuoteElement", "cite", "cite", Reflect::Url),
    ("HTMLModElement", "cite", "cite", Reflect::Url),
    ("HTMLModElement", "dateTime", "datetime", Reflect::Str),
    ("HTMLProgressElement", "max", "max", Reflect::Str),
    ("HTMLEmbedElement", "src", "src", Reflect::Url),
    ("HTMLEmbedElement", "type", "type", Reflect::Str),
    ("HTMLObjectElement", "data", "data", Reflect::Url),
    ("HTMLObjectElement", "type", "type", Reflect::Str),
    ("HTMLObjectElement", "name", "name", Reflect::Str),
    ("HTMLTrackElement", "src", "src", Reflect::Url),
    ("HTMLSlotElement", "name", "name", Reflect::Str),
    ("HTMLFieldSetElement", "disabled", "disabled", Reflect::Bool),
    ("HTMLFieldSetElement", "name", "name", Reflect::Str),
    ("HTMLOutputElement", "name", "name", Reflect::Str),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interfaces_exist_in_table() {
        let t = &crate::generated::chromium::TABLE;
        for (iface, ..) in REFLECTED {
            assert!(t.interface(iface).is_some(), "{iface}");
        }
        for l in ["a", "div", "td", "ins", "foo", "x-y", "section", "h3"] {
            assert!(t.interface(html_interface(l)).is_some(), "{l}");
        }
    }
}
