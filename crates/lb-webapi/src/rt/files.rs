//! `File`, `FileList`, `DataTransfer` (with its item list) and the files of
//! `<input type=file>`.
//!
//! Playwright's `setInputFiles` builds `File`s in its utility world, adds
//! them to a `new DataTransfer()` and assigns `dt.files` to `input.files`;
//! with local paths it sends `DOM.setFileInputFiles` instead, which the
//! page layer turns into [`set_input_files`]. A form posts the chosen
//! files' bytes (`form.rs`).
//!
//! As in Chrome (checked in Chrome for Testing 1243): `dataTransfer.files`
//! is the same `FileList` on every read (kept up to date with the items),
//! `items` is a new object each time, a file input's `files` is the same
//! object until it is replaced, and its `value` is
//! `C:\fakepath\<first name>`.

use std::rc::Rc;

use lb_dom::NodeId;
use lb_js::{Arg, CallCtx, HostError, HostRef, ValueKind};

use super::misc::{BlobData, blob_parts};
use super::{
    Dict, K_OBJ, Obj, Runtime, TaskSource, Window, ctor, getter, illegal, method, need_args,
    opt_string, setter, this_obj,
};
use crate::registry::Registry;

/// A `File`'s own fields; its bytes and type are the `Blob`'s.
#[derive(Clone, Debug)]
pub(crate) struct FileMeta {
    pub name: String,
    /// ms since the epoch.
    pub last_modified: f64,
}

/// One entry of a `DataTransfer`'s item list.
#[derive(Clone, Debug)]
pub(crate) enum DtItem {
    Text {
        type_: String,
        data: String,
    },
    /// A `File` (object id).
    File(u32),
}

/// A drag data store's mode (HTML): who may read and change it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DtMode {
    /// `dragstart`, and `new DataTransfer()`.
    ReadWrite,
    /// `drop`: data readable, nothing changes.
    ReadOnly,
    /// Other drag events: only the types are visible.
    Protected,
}

/// A `DataTransfer`'s drag data store.
#[derive(Clone, Debug)]
pub(crate) struct DataTransferData {
    pub items: Vec<DtItem>,
    pub drop_effect: String,
    pub effect_allowed: String,
    pub mode: DtMode,
}

impl Default for DataTransferData {
    fn default() -> Self {
        DataTransferData {
            items: Vec::new(),
            drop_effect: "none".into(),
            effect_allowed: "none".into(),
            mode: DtMode::ReadWrite,
        }
    }
}

/// A new `DataTransfer` for a drag (its store in `mode`).
pub(crate) fn new_drag_data(w: &Window, d: DataTransferData) -> u32 {
    w.new_obj(Obj::DataTransfer(Box::new(d)))
}

/// Change a drag data store's mode (or `effectAllowed`, `dropEffect`).
pub(crate) fn update_drag_data(w: &Window, id: u32, f: impl FnOnce(&mut DataTransferData)) {
    with_dt(w, id, f);
}

/// A copy of a drag data store.
pub(crate) fn drag_data(w: &Window, id: u32) -> Option<DataTransferData> {
    with_dt(w, id, |d| d.clone())
}

/// A new `File` object; returns its id.
pub(crate) fn new_file(
    w: &Window,
    bytes: Vec<u8>,
    type_: &str,
    name: &str,
    last_modified: f64,
) -> u32 {
    w.new_obj(Obj::Blob(Box::new(BlobData {
        bytes,
        type_: blob_type(type_),
        file: Some(FileMeta {
            name: name.to_owned(),
            last_modified,
        }),
    })))
}

/// A `FileList` of these `File` ids.
pub(crate) fn new_file_list(w: &Window, files: Vec<u32>) -> u32 {
    w.new_obj(Obj::FileList(files))
}

/// `BlobPropertyBag.type`: lowercased, or "" if it has characters outside
/// U+0020-U+007E.
fn blob_type(t: &str) -> String {
    if t.chars().all(|c| ('\u{20}'..='\u{7e}').contains(&c)) {
        t.to_ascii_lowercase()
    } else {
        String::new()
    }
}

/// ms since the epoch, as `Date.now()` reads it.
fn now_ms(w: &Window) -> f64 {
    (w.clock.time_origin + w.clock.now()).floor()
}

/// The `File` behind object `id`: (bytes, type, name, lastModified).
pub(crate) fn file_parts(w: &Window, id: u32) -> Option<(Vec<u8>, String, String, f64)> {
    w.with_obj(id, |o| match o {
        Obj::Blob(b) => b.file.as_ref().map(|f| {
            (
                b.bytes.clone(),
                b.type_.clone(),
                f.name.clone(),
                f.last_modified,
            )
        }),
        _ => None,
    })
    .flatten()
}

fn is_file(w: &Window, id: u32) -> bool {
    file_parts(w, id).is_some()
}

/// The `File` id of argument `i`, if it is one.
fn arg_file(cx: &mut CallCtx<'_, '_, '_>, w: &Window, i: usize) -> Option<u32> {
    if i >= cx.len() || cx.arg_kind(i) != ValueKind::Object {
        return None;
    }
    let h = cx.arg_handle(i);
    let id = cx.handle_host(h).filter(|h| h.kind == K_OBJ).map(|h| h.id);
    cx.release(h);
    id.filter(|&id| is_file(w, id))
}

fn list_files(w: &Window, id: u32) -> Option<Vec<u32>> {
    w.with_obj(id, |o| match o {
        Obj::FileList(f) => Some(f.clone()),
        _ => None,
    })
    .flatten()
}

fn this_file(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(String, f64), HostError> {
    let id = this_obj(cx)?;
    file_parts(w, id)
        .map(|(_, _, n, m)| (n, m))
        .ok_or_else(illegal)
}

fn this_list(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<Vec<u32>, HostError> {
    let id = this_obj(cx)?;
    list_files(w, id).ok_or_else(illegal)
}

fn this_dt(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<u32, HostError> {
    let id = this_obj(cx)?;
    let ok = w
        .with_obj(id, |o| matches!(o, Obj::DataTransfer(_)))
        .unwrap_or(false);
    if ok { Ok(id) } else { Err(illegal()) }
}

fn with_dt<R>(w: &Window, id: u32, f: impl FnOnce(&mut DataTransferData) -> R) -> Option<R> {
    w.with_obj(id, |o| match o {
        Obj::DataTransfer(d) => Some(f(d)),
        _ => None,
    })
    .flatten()
}

/// The `DataTransfer` of a `DataTransferItemList` (`this`).
fn this_items(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<u32, HostError> {
    let id = this_obj(cx)?;
    w.with_obj(id, |o| match o {
        Obj::DataTransferItemList(dt) => Some(*dt),
        _ => None,
    })
    .flatten()
    .ok_or_else(illegal)
}

fn item_at(w: &Window, dt: u32, i: usize) -> Option<DtItem> {
    with_dt(w, dt, |d| d.items.get(i).cloned()).flatten()
}

/// `getData`/`setData` format: lowercased, `text` is `text/plain`, `url`
/// is `text/uri-list`.
fn normalize_format(f: &str) -> String {
    let f = f.to_ascii_lowercase();
    match f.as_str() {
        "text" => "text/plain".into(),
        "url" => "text/uri-list".into(),
        _ => f,
    }
}

/// A file input's `FileList` id, made (empty) on first use.
pub(crate) fn input_file_list(w: &Window, slot: u32, n: NodeId) -> u32 {
    if let Some(id) = w.input_files.borrow().get(&(slot, n)) {
        return *id;
    }
    let id = new_file_list(w, Vec::new());
    w.input_files.borrow_mut().insert((slot, n), id);
    id
}

/// The `File` ids chosen in a file input.
pub(crate) fn input_files(w: &Window, slot: u32, n: NodeId) -> Vec<u32> {
    let id = w.input_files.borrow().get(&(slot, n)).copied();
    id.and_then(|id| list_files(w, id)).unwrap_or_default()
}

/// A file input's `value`: `C:\fakepath\` and the first file's name, or "".
pub(crate) fn input_value(w: &Window, slot: u32, n: NodeId) -> String {
    input_files(w, slot, n)
        .first()
        .and_then(|&f| file_parts(w, f))
        .map(|(_, _, name, _)| format!("C:\\fakepath\\{name}"))
        .unwrap_or_default()
}

/// Set a file input's `value` (only "" is allowed: it clears the files).
pub(crate) fn set_input_value(w: &Window, slot: u32, n: NodeId, v: &str) -> Result<(), HostError> {
    if !v.is_empty() {
        return Err(w.rt.err(
            "InvalidStateError",
            "Failed to set the 'value' property on 'HTMLInputElement': This input element accepts a filename, which may only be programmatically set to the empty string.",
        ));
    }
    let id = new_file_list(w, Vec::new());
    w.input_files.borrow_mut().insert((slot, n), id);
    Ok(())
}

/// Choose `files` (`(bytes, type, name, lastModified)`) in a file input,
/// as `DOM.setFileInputFiles` does; the caller fires `input` and `change`.
pub fn set_input_files(
    w: &Window,
    slot: u32,
    n: NodeId,
    files: Vec<(Vec<u8>, String, String, f64)>,
) {
    let ids = files
        .into_iter()
        .map(|(bytes, ty, name, lm)| new_file(w, bytes, &ty, &name, lm))
        .collect();
    let list = new_file_list(w, ids);
    w.input_files.borrow_mut().insert((slot, n), list);
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    // File
    ctor(reg, rt, "File", |cx, w| {
        need_args(cx, 2, "construct", "File")?;
        let bytes = blob_parts(cx, w, 0)?;
        let name = cx.arg_string(1)?;
        let d = Dict::arg(cx, 2);
        let ty = d.string(cx, "type")?.unwrap_or_default();
        let lm = d.number(cx, "lastModified")?;
        d.release(cx);
        let lm = lm.map_or_else(
            || now_ms(w),
            |v| if v.is_finite() { v.trunc() } else { 0.0 },
        );
        let id = new_file(w, bytes, &ty, &name, lm);
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    getter(reg, rt, "File", "name", |cx, w| {
        let (n, _) = this_file(cx, w)?;
        cx.ret(n)
    });
    getter(reg, rt, "File", "lastModified", |cx, w| {
        let (_, m) = this_file(cx, w)?;
        cx.ret(m)
    });
    getter(reg, rt, "File", "webkitRelativePath", |cx, w| {
        this_file(cx, w)?;
        cx.ret("")
    });

    // FileList
    getter(reg, rt, "FileList", "length", |cx, w| {
        let f = this_list(cx, w)?;
        cx.ret(f.len() as f64)
    });
    method(reg, rt, "FileList", "item", |cx, w| {
        need_args(cx, 1, "item", "FileList")?;
        let f = this_list(cx, w)?;
        let i = cx.arg_f64(0)?;
        match f.get(i as usize).filter(|_| i >= 0.0) {
            Some(&id) => cx.ret(w.obj_arg(id)),
            None => cx.ret(Arg::Null),
        }
    });
    {
        let r = rt.clone();
        reg.indexed_getter(
            "FileList",
            Rc::new(move |cx, i| {
                let Some(w) = r.window(cx.holder_realm()) else {
                    return Ok(None);
                };
                let Some(h) = cx.holder_host().filter(|h| h.kind == K_OBJ) else {
                    return Ok(None);
                };
                Ok(list_files(&w, h.id)
                    .and_then(|f| f.get(i as usize).copied())
                    .map(|id| w.obj_arg(id)))
            }),
        );
        let r = rt.clone();
        reg.indexed_length(
            "FileList",
            Rc::new(move |cx| {
                let Some(w) = r.window(cx.holder_realm()) else {
                    return 0;
                };
                cx.holder_host()
                    .filter(|h| h.kind == K_OBJ)
                    .and_then(|h| list_files(&w, h.id))
                    .map_or(0, |f| f.len() as u32)
            }),
        );
    }

    // DataTransfer
    ctor(reg, rt, "DataTransfer", |cx, w| {
        let id = w.new_obj(Obj::DataTransfer(Box::default()));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    getter(reg, rt, "DataTransfer", "items", |cx, w| {
        let dt = this_dt(cx, w)?;
        let id = w.new_obj(Obj::DataTransferItemList(dt));
        cx.ret(w.obj_arg(id))
    });
    getter(reg, rt, "DataTransfer", "files", |cx, w| {
        let dt = this_dt(cx, w)?;
        let files = with_dt(w, dt, |d| {
            d.items
                .iter()
                .filter_map(|i| match i {
                    DtItem::File(f) => Some(*f),
                    DtItem::Text { .. } => None,
                })
                .collect()
        })
        .unwrap_or_default();
        let id = w.cached_obj((dt, 0, "DataTransfer.files", String::new()), || {
            Obj::FileList(Vec::new())
        });
        w.with_obj(id, |o| {
            if let Obj::FileList(f) = o {
                *f = files;
            }
        });
        cx.ret(w.obj_arg(id))
    });
    getter(reg, rt, "DataTransfer", "types", |cx, w| {
        let dt = this_dt(cx, w)?;
        let types = with_dt(w, dt, |d| {
            let mut t: Vec<Arg> = d
                .items
                .iter()
                .filter_map(|i| match i {
                    DtItem::Text { type_, .. } => Some(Arg::String(type_.clone())),
                    DtItem::File(_) => None,
                })
                .collect();
            if d.items.iter().any(|i| matches!(i, DtItem::File(_))) {
                t.push(Arg::String("Files".into()));
            }
            t
        })
        .unwrap_or_default();
        cx.ret(Arg::Array(types))
    });
    method(reg, rt, "DataTransfer", "getData", |cx, w| {
        need_args(cx, 1, "getData", "DataTransfer")?;
        let dt = this_dt(cx, w)?;
        let f = normalize_format(&cx.arg_string(0)?);
        let data = with_dt(w, dt, |d| {
            if d.mode == DtMode::Protected {
                return None;
            }
            d.items.iter().find_map(|i| match i {
                DtItem::Text { type_, data } if *type_ == f => Some(data.clone()),
                _ => None,
            })
        })
        .flatten()
        .unwrap_or_default();
        // `URL` reads the first URL of a text/uri-list.
        let data = if f == "text/uri-list" {
            data.lines()
                .find(|l| !l.starts_with('#') && !l.trim().is_empty())
                .unwrap_or("")
                .to_owned()
        } else {
            data
        };
        cx.ret(data)
    });
    method(reg, rt, "DataTransfer", "setData", |cx, w| {
        need_args(cx, 2, "setData", "DataTransfer")?;
        let dt = this_dt(cx, w)?;
        let f = normalize_format(&cx.arg_string(0)?);
        let data = cx.arg_string(1)?;
        with_dt(w, dt, |d| {
            if d.mode != DtMode::ReadWrite {
                return;
            }
            d.items
                .retain(|i| !matches!(i, DtItem::Text { type_, .. } if *type_ == f));
            d.items.push(DtItem::Text { type_: f, data });
        });
        Ok(())
    });
    method(reg, rt, "DataTransfer", "clearData", |cx, w| {
        let dt = this_dt(cx, w)?;
        let f = opt_string(cx, 0)?.map(|f| normalize_format(&f));
        with_dt(w, dt, |d| {
            if d.mode != DtMode::ReadWrite {
                return;
            }
            d.items.retain(|i| match i {
                DtItem::Text { type_, .. } => f.as_ref().is_some_and(|f| f != type_),
                DtItem::File(_) => true,
            })
        });
        Ok(())
    });
    method(reg, rt, "DataTransfer", "setDragImage", |cx, w| {
        need_args(cx, 3, "setDragImage", "DataTransfer")?;
        this_dt(cx, w)?;
        Ok(())
    });
    for (prop, allowed) in [
        ("dropEffect", &["none", "copy", "link", "move"][..]),
        (
            "effectAllowed",
            &[
                "none",
                "copy",
                "copyLink",
                "copyMove",
                "link",
                "linkMove",
                "move",
                "all",
                "uninitialized",
            ][..],
        ),
    ] {
        getter(reg, rt, "DataTransfer", prop, move |cx, w| {
            let dt = this_dt(cx, w)?;
            let v = with_dt(w, dt, |d| {
                if prop == "dropEffect" {
                    d.drop_effect.clone()
                } else {
                    d.effect_allowed.clone()
                }
            })
            .unwrap_or_default();
            cx.ret(v)
        });
        setter(reg, rt, "DataTransfer", prop, move |cx, w| {
            let dt = this_dt(cx, w)?;
            let v = cx.arg_string(0)?;
            // Other values are ignored, as in Blink.
            if allowed.contains(&v.as_str()) {
                with_dt(w, dt, |d| {
                    if prop == "dropEffect" {
                        d.drop_effect = v;
                    } else {
                        d.effect_allowed = v;
                    }
                });
            }
            Ok(())
        });
    }

    // DataTransferItemList
    getter(reg, rt, "DataTransferItemList", "length", |cx, w| {
        let dt = this_items(cx, w)?;
        let n = with_dt(w, dt, |d| d.items.len()).unwrap_or(0);
        cx.ret(n as f64)
    });
    method(reg, rt, "DataTransferItemList", "add", |cx, w| {
        need_args(cx, 1, "add", "DataTransferItemList")?;
        let dt = this_items(cx, w)?;
        let item = if let Some(f) = arg_file(cx, w, 0) {
            DtItem::File(f)
        } else {
            if cx.len() < 2 {
                return Err(HostError::type_error(
                    "Failed to execute 'add' on 'DataTransferItemList': parameter 1 is not of type 'File'.",
                ));
            }
            let data = cx.arg_string(0)?;
            let type_ = cx.arg_string(1)?.to_ascii_lowercase();
            let exists = with_dt(w, dt, |d| {
                d.items
                    .iter()
                    .any(|i| matches!(i, DtItem::Text { type_: t, .. } if *t == type_))
            })
            .unwrap_or(false);
            if exists {
                return Err(w.rt.err(
                    "NotSupportedError",
                    format!("Failed to execute 'add' on 'DataTransferItemList': An item already exists for type '{type_}'."),
                ));
            }
            DtItem::Text { type_, data }
        };
        let index = with_dt(w, dt, |d| {
            d.items.push(item);
            d.items.len() - 1
        })
        .unwrap_or(0);
        let id = w.new_obj(Obj::DataTransferItem { dt, index });
        cx.ret(w.obj_arg(id))
    });
    method(reg, rt, "DataTransferItemList", "remove", |cx, w| {
        need_args(cx, 1, "remove", "DataTransferItemList")?;
        let dt = this_items(cx, w)?;
        let i = cx.arg_f64(0)?;
        with_dt(w, dt, |d| {
            if i >= 0.0 && (i as usize) < d.items.len() {
                d.items.remove(i as usize);
            }
        });
        Ok(())
    });
    method(reg, rt, "DataTransferItemList", "clear", |cx, w| {
        let dt = this_items(cx, w)?;
        with_dt(w, dt, |d| d.items.clear());
        Ok(())
    });
    {
        let r = rt.clone();
        reg.indexed_getter(
            "DataTransferItemList",
            Rc::new(move |cx, i| {
                let Some(w) = r.window(cx.holder_realm()) else {
                    return Ok(None);
                };
                let Some(h) = cx.holder_host().filter(|h| h.kind == K_OBJ) else {
                    return Ok(None);
                };
                let dt = w
                    .with_obj(h.id, |o| match o {
                        Obj::DataTransferItemList(dt) => Some(*dt),
                        _ => None,
                    })
                    .flatten();
                Ok(dt
                    .filter(|&dt| item_at(&w, dt, i as usize).is_some())
                    .map(|dt| {
                        let id = w.new_obj(Obj::DataTransferItem {
                            dt,
                            index: i as usize,
                        });
                        w.obj_arg(id)
                    }))
            }),
        );
        let r = rt.clone();
        reg.indexed_length(
            "DataTransferItemList",
            Rc::new(move |cx| {
                let Some(w) = r.window(cx.holder_realm()) else {
                    return 0;
                };
                cx.holder_host()
                    .filter(|h| h.kind == K_OBJ)
                    .and_then(|h| {
                        w.with_obj(h.id, |o| match o {
                            Obj::DataTransferItemList(dt) => Some(*dt),
                            _ => None,
                        })
                        .flatten()
                    })
                    .and_then(|dt| with_dt(&w, dt, |d| d.items.len() as u32))
                    .unwrap_or(0)
            }),
        );
    }

    // DataTransferItem
    fn this_item(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<Option<DtItem>, HostError> {
        let id = this_obj(cx)?;
        let (dt, index) = w
            .with_obj(id, |o| match o {
                Obj::DataTransferItem { dt, index } => Some((*dt, *index)),
                _ => None,
            })
            .flatten()
            .ok_or_else(illegal)?;
        Ok(item_at(w, dt, index))
    }
    getter(reg, rt, "DataTransferItem", "kind", |cx, w| {
        let kind = match this_item(cx, w)? {
            Some(DtItem::File(_)) => "file",
            Some(DtItem::Text { .. }) => "string",
            None => "",
        };
        cx.ret(kind)
    });
    getter(reg, rt, "DataTransferItem", "type", |cx, w| {
        let t = match this_item(cx, w)? {
            Some(DtItem::File(f)) => file_parts(w, f).map(|p| p.1).unwrap_or_default(),
            Some(DtItem::Text { type_, .. }) => type_,
            None => String::new(),
        };
        cx.ret(t)
    });
    method(
        reg,
        rt,
        "DataTransferItem",
        "getAsFile",
        |cx, w| match this_item(cx, w)? {
            Some(DtItem::File(f)) => cx.ret(w.obj_arg(f)),
            _ => cx.ret(Arg::Null),
        },
    );
    method(reg, rt, "DataTransferItem", "getAsString", |cx, w| {
        let item = this_item(cx, w)?;
        if let (Some(DtItem::Text { data, .. }), true) = (
            item,
            !cx.is_empty() && cx.arg_kind(0) == ValueKind::Function,
        ) {
            let f = cx.arg_handle(0);
            w.queue_task(
                TaskSource::DomManipulation,
                Box::new(move |js, w| {
                    if let Err(e) = js.call(f, Arg::Undefined, &[Arg::String(data)]) {
                        w.report_exception(js, &e);
                    }
                    js.release(f);
                }),
            );
        }
        Ok(())
    });
}
