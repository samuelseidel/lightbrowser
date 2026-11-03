//! A compact UA stylesheet for HTML, following Chromium's `html.css` for the
//! properties we compute.

use crate::media::OsKind;

const COMMON: &str = r#"
html, address, blockquote, body, center, dialog, div, figure, figcaption,
footer, form, header, hr, legend, listing, main, p, plaintext, pre, search,
xmp, article, aside, h1, h2, h3, h4, h5, h6, hgroup, nav, section, dir, dd,
dl, dt, menu, ol, ul, fieldset, details, summary, optgroup, frameset, frame,
address { display: block }
head, link, meta, script, style, title, base, template, noscript, area,
datalist, param, source, track, noembed, noframes, [hidden] { display: none }
input[type=hidden i] { display: none !important }
dialog:not([open]) { display: none }
li { display: list-item; text-align: match-parent }
body { margin: 8px }
p, blockquote, figure, dl, multicol { margin: 1em 0 }
blockquote, figure { margin-left: 40px; margin-right: 40px }
address, i, cite, em, var, dfn { font-style: italic }
center { text-align: -webkit-center }
hr { color: gray; border-style: inset; border-width: 1px; margin: 0.5em auto; overflow: hidden }
h1 { font-size: 2em; margin: 0.67em 0; font-weight: bold }
h2 { font-size: 1.5em; margin: 0.83em 0; font-weight: bold }
h3 { font-size: 1.17em; margin: 1em 0; font-weight: bold }
h4 { margin: 1.33em 0; font-weight: bold }
h5 { font-size: 0.83em; margin: 1.67em 0; font-weight: bold }
h6 { font-size: 0.67em; margin: 2.33em 0; font-weight: bold }
:is(article, aside, nav, section) h1 { font-size: 1.5em; margin: 0.83em 0 }
ul, menu, dir, ol { margin: 1em 0; padding-left: 40px }
:is(ul, ol, menu, dir) :is(ul, ol, menu, dir) { margin-top: 0; margin-bottom: 0 }
dd { margin-left: 40px }
table { display: table; box-sizing: border-box; border-color: gray; text-indent: initial }
caption { display: table-caption; text-align: -webkit-center }
thead { display: table-header-group }
tbody { display: table-row-group }
tfoot { display: table-footer-group }
tr { display: table-row }
col { display: table-column }
colgroup { display: table-column-group }
td, th { display: table-cell; padding: 1px }
th { font-weight: bold; text-align: -internal-center }
b, strong { font-weight: bolder }
small { font-size: smaller }
big { font-size: larger }
sub, sup { font-size: smaller }
pre, xmp, plaintext, listing { white-space: pre; margin: 1em 0 }
pre, code, kbd, samp, tt, xmp, plaintext, listing { font-family: monospace }
nobr { white-space: nowrap }
a:any-link { color: -webkit-link; cursor: pointer }
mark { background-color: mark; color: marktext }
img, svg, video, canvas, audio, iframe, embed, object { display: inline }
iframe { border: 2px inset }
video { object-fit: contain }
audio:not([controls]) { display: none }
fieldset { margin: 0 2px; padding: 0.35em 0.75em 0.625em; border: 2px groove threedface; min-width: min-content }
legend { padding: 0 2px }
input, textarea, select, button { margin: 0; letter-spacing: normal; word-spacing: normal; text-align: start; display: inline-block; color: fieldtext }
input { padding: 1px 2px; border: 2px inset; background-color: field; cursor: text }
input[type=checkbox i], input[type=radio i] { padding: 0; border: initial; background-color: initial; cursor: default; box-sizing: border-box }
input[type=checkbox i] { margin: 3px 3px 3px 4px }
input[type=radio i] { margin: 3px 3px 0 5px }
input[type=range i] { margin: 2px; padding: initial; border: initial; cursor: default; color: #101010 }
input[type=color i] { padding: 1px 2px; background-color: buttonface; border: 1px solid buttonborder; cursor: default; box-sizing: border-box }
input[type=file i] { padding: initial; border: initial; background-color: initial; cursor: default }
input[type=image i] { padding: initial; border: initial; background-color: initial; cursor: pointer }
input:is([type=button i], [type=submit i], [type=reset i]), button {
  padding: 1px 6px; border: 2px outset buttonborder; background-color: buttonface;
  color: buttontext; text-align: center; box-sizing: border-box; cursor: default }
textarea { padding: 2px; border: 1px solid; background-color: field; white-space: pre-wrap; cursor: text; font-family: monospace }
select { border: 1px solid; box-sizing: border-box; background-color: field; white-space: pre; cursor: default }
option { display: block; white-space: nowrap; padding: 0 2px 1px }
meter { display: inline-block; box-sizing: border-box; vertical-align: -0.2em }
progress { display: inline-block; box-sizing: border-box; vertical-align: -0.2em }
ruby { display: ruby }
rt { display: ruby-text }
"#;

/// The UA stylesheet for an OS. Form controls use Arial 13.333px on
/// Windows and Linux and `system-ui` on macOS, as Chrome does.
pub fn ua_stylesheet(os: OsKind) -> String {
    let control_font = match os {
        OsKind::MacOs => "system-ui",
        _ => "Arial",
    };
    format!(
        "{COMMON}\ninput, select, button {{ font: 400 13.333333px {control_font} }}\n\
         textarea {{ font-size: 13.333333px }}\n"
    )
}
