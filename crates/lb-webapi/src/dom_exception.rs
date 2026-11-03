//! `DOMException`: legacy codes, a throwable factory for host functions, and
//! the default implementation (constructor, `name`, `message`, `code`).

/// Legacy `code` for a DOMException name (0 for names without one).
pub fn legacy_code(name: &str) -> u16 {
    match name {
        "IndexSizeError" => 1,
        "HierarchyRequestError" => 3,
        "WrongDocumentError" => 4,
        "InvalidCharacterError" => 5,
        "NoModificationAllowedError" => 7,
        "NotFoundError" => 8,
        "NotSupportedError" => 9,
        "InUseAttributeError" => 10,
        "InvalidStateError" => 11,
        "SyntaxError" => 12,
        "InvalidModificationError" => 13,
        "NamespaceError" => 14,
        "InvalidAccessError" => 15,
        "TypeMismatchError" => 17,
        "SecurityError" => 18,
        "NetworkError" => 19,
        "AbortError" => 20,
        "URLMismatchError" => 21,
        "QuotaExceededError" => 22,
        "TimeoutError" => 23,
        "InvalidNodeTypeError" => 24,
        "DataCloneError" => 25,
        _ => 0,
    }
}

#[cfg(feature = "chrome")]
pub use imp::*;

#[cfg(feature = "chrome")]
mod imp {
    use std::cell::Cell;
    use std::rc::Rc;

    use lb_js::{Arg, HostError, InterfaceId, JsValue, ValueKind};

    use super::legacy_code;
    use crate::registry::{ImplKind, Registry, host_fn};

    const NAME_KEY: &str = "lb-webapi:DOMException#name";
    const MESSAGE_KEY: &str = "lb-webapi:DOMException#message";

    /// Makes throwable `DOMException`s from host functions:
    /// `return Err(dom.error("NotFoundError", "The node ..."))`. Clone it
    /// from [`Registry::dom_exceptions`] into closures before the bindings
    /// are defined; it points at the `DOMException` interface afterwards.
    /// Before that it falls back to a plain `Error`.
    #[derive(Clone, Default, Debug)]
    pub struct DomExceptions(Rc<Cell<Option<InterfaceId>>>);

    impl DomExceptions {
        /// A `DOMException` with `name` and `message`, built in the realm of
        /// the call that throws it.
        pub fn error(&self, name: &str, message: impl Into<String>) -> HostError {
            let message = message.into();
            match self.0.get() {
                Some(id) => HostError::Throw(Arg::Construct(
                    id,
                    vec![Arg::String(message), Arg::String(name.to_owned())],
                )),
                None => HostError::Error(message),
            }
        }

        pub(crate) fn set_interface(&self, id: InterfaceId) {
            self.0.set(Some(id));
        }

        /// The `DOMException` interface, once bindings are defined.
        pub fn interface(&self) -> Option<InterfaceId> {
            self.0.get()
        }
    }

    fn string_arg(
        cx: &mut lb_js::CallCtx<'_, '_, '_>,
        i: usize,
        default: &str,
    ) -> Result<String, HostError> {
        if cx.len() <= i || cx.arg_kind(i) == ValueKind::Undefined {
            Ok(default.to_owned())
        } else {
            cx.arg_string(i)
        }
    }

    fn private_string(cx: &mut lb_js::CallCtx<'_, '_, '_>, key: &str, default: &str) -> String {
        match cx.this_private(key) {
            JsValue::String(s) => s,
            _ => default.to_owned(),
        }
    }

    /// Register the default `DOMException` members unless already
    /// registered. State lives in private properties of each exception
    /// object, so it is freed with it.
    pub fn register_defaults(reg: &mut Registry) {
        if reg.get("DOMException", "", ImplKind::Constructor).is_none() {
            reg.constructor(
                "DOMException",
                host_fn(|cx| {
                    let message = string_arg(cx, 0, "")?;
                    let name = string_arg(cx, 1, "Error")?;
                    cx.set_this_private(MESSAGE_KEY, message)?;
                    cx.set_this_private(NAME_KEY, name)
                }),
            );
        }
        if reg.get("DOMException", "name", ImplKind::Getter).is_none() {
            reg.getter(
                "DOMException",
                "name",
                host_fn(|cx| {
                    let v = private_string(cx, NAME_KEY, "Error");
                    cx.ret(v)
                }),
            );
        }
        if reg
            .get("DOMException", "message", ImplKind::Getter)
            .is_none()
        {
            reg.getter(
                "DOMException",
                "message",
                host_fn(|cx| {
                    let v = private_string(cx, MESSAGE_KEY, "");
                    cx.ret(v)
                }),
            );
        }
        if reg.get("DOMException", "code", ImplKind::Getter).is_none() {
            reg.getter(
                "DOMException",
                "code",
                host_fn(|cx| {
                    let name = private_string(cx, NAME_KEY, "Error");
                    cx.ret(u32::from(legacy_code(&name)))
                }),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::legacy_code;

    #[test]
    fn codes() {
        assert_eq!(legacy_code("NotSupportedError"), 9);
        assert_eq!(legacy_code("DataCloneError"), 25);
        assert_eq!(legacy_code("NotAllowedError"), 0);
    }
}
