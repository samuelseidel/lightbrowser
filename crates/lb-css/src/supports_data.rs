//! Chrome's property grammars for `CSS.supports()`: `supports.grammar`,
//! one `name = syntax` line per property Chrome 153 knows and
//! `<name> = syntax` per type (CSS value definition syntax). Seeded from
//! the specs' grammars (@webref/css, MIT) and edited by hand until Chrome
//! 153 gives the same answers on `lb-page/tests/css_supports/corpus.jsonl`
//! (`tools/css_supports/`: `gen_cases.py` makes cases, `record.py
//! --corpus` records Chrome's answers).

pub(crate) static GRAMMAR: &str = include_str!("supports.grammar");
