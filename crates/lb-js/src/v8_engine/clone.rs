//! Structured clone (HTML "StructuredSerializeWithTransfer") on V8's own
//! `ValueSerializer`, as Blink does it: the wire format, cycles, `Date`,
//! `RegExp`, `Map`, `Set`, typed arrays, errors and V8's own error texts
//! ("... could not be cloned.") are V8's. What the host adds:
//!
//! - **Host objects** (wrappers with host fields): the host decides how
//!   each travels (`MessagePort` by transfer, `Blob` by copy, anything
//!   else is refused with Chrome's `X object could not be cloned.`); the
//!   stream carries the index the host returns. On the other side the
//!   host passes the objects to put back, by index, as `(interface, host
//!   ref)` pairs, and they are wrapped in the receiving realm.
//! - **Transferred `ArrayBuffer`s**: detached on the sending side; their
//!   bytes travel beside the stream ([`Serialized::buffers`]) and become
//!   new buffers on the receiving side. Copied rather than handed over:
//!   a V8 backing store is not `Sync`, so it cannot move to the isolate
//!   of another thread without `unsafe`.
//!
//! [`Serialized`] is plain data (`Send`): it crosses threads to workers.

use std::cell::RefCell;
use std::rc::Rc;

use v8::{ValueDeserializerHelper, ValueSerializerHelper};

use super::{SharedState, load_handle, read_bytes, read_host, store_handle, wrap_host};
use crate::{HostRef, InterfaceId, RealmId};

/// A structured-clone serialization: V8's wire format plus the contents of
/// the `ArrayBuffer`s it transferred (by transfer index).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Serialized {
    pub data: Vec<u8>,
    pub buffers: Vec<Vec<u8>>,
}

/// Decides how a host object travels: `Ok(index)` writes the index into
/// the stream (the host keeps what the index means), `Err(message)`
/// refuses with that `DataCloneError` message. Gets the host ref and the
/// object's class name (`MessagePort`, `HTMLDivElement`).
pub type HostWriter<'h> = &'h mut dyn FnMut(HostRef, &str) -> Result<u32, String>;

struct SerDelegate<'h> {
    error: Rc<RefCell<Option<String>>>,
    host: RefCell<HostWriter<'h>>,
}

impl v8::ValueSerializerImpl for SerDelegate<'_> {
    fn throw_data_clone_error<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        message: v8::Local<'s, v8::String>,
    ) {
        *self.error.borrow_mut() = Some(message.to_rust_string_lossy(scope));
        let e = v8::Exception::error(scope, message);
        scope.throw_exception(e);
    }

    fn write_host_object<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        object: v8::Local<'s, v8::Object>,
        helper: &dyn v8::ValueSerializerHelper,
    ) -> Option<bool> {
        let class = object.get_constructor_name().to_rust_string_lossy(scope);
        let outcome = match read_host(scope, object) {
            Some(h) => (self.host.borrow_mut())(h, &class),
            None => Err(format!("{class} object could not be cloned.")),
        };
        match outcome {
            Ok(index) => {
                helper.write_uint32(index);
                Some(true)
            }
            Err(msg) => {
                let m = v8::String::new(scope, &msg)?;
                self.throw_data_clone_error(scope, m);
                None
            }
        }
    }
}

struct DeDelegate {
    state: SharedState,
    realm: RealmId,
    hosts: Vec<(InterfaceId, HostRef)>,
}

impl v8::ValueDeserializerImpl for DeDelegate {
    fn read_host_object<'s>(
        &self,
        scope: &mut v8::PinScope<'s, '_>,
        helper: &dyn v8::ValueDeserializerHelper,
    ) -> Option<v8::Local<'s, v8::Object>> {
        let mut index = 0u32;
        let found = helper
            .read_uint32(&mut index)
            .then(|| self.hosts.get(index as usize))
            .flatten();
        let Some(&(iface, host)) = found else {
            let m = v8::String::new(scope, "Unable to deserialize cloned data.")?;
            let e = v8::Exception::error(scope, m);
            scope.throw_exception(e);
            return None;
        };
        wrap_host(scope, &self.state, self.realm, iface, host).ok()
    }
}

/// Serialize `value` in the current context, transferring `transfer`
/// (`ArrayBuffer`s; anything else is the host's business). Errors are
/// `DataCloneError` messages.
pub(super) fn serialize_in(
    scope: &mut v8::PinScope<'_, '_>,
    value: v8::Local<'_, v8::Value>,
    transfer: &[v8::Local<'_, v8::ArrayBuffer>],
    host: HostWriter<'_>,
) -> Result<Serialized, String> {
    for (i, ab) in transfer.iter().enumerate() {
        let v: v8::Local<v8::Value> = (*ab).into();
        if transfer[..i].iter().any(|x| {
            let x: v8::Local<v8::Value> = (*x).into();
            x.strict_equals(v)
        }) {
            return Err(format!(
                "ArrayBuffer at index {i} is a duplicate of an earlier ArrayBuffer."
            ));
        }
        if ab.was_detached() {
            return Err(format!("ArrayBuffer at index {i} is already detached."));
        }
        if !ab.is_detachable() {
            return Err(format!("ArrayBuffer at index {i} is not detachable."));
        }
    }
    let error = Rc::new(RefCell::new(None));
    v8::tc_scope!(let tc, scope);
    let data = {
        let ser = v8::ValueSerializer::new(
            tc,
            Box::new(SerDelegate {
                error: error.clone(),
                host: RefCell::new(host),
            }),
        );
        ser.write_header();
        for (i, ab) in transfer.iter().enumerate() {
            ser.transfer_array_buffer(i as u32, *ab);
        }
        let context = tc.get_current_context();
        if ser.write_value(context, value) != Some(true) {
            let msg = error.borrow_mut().take();
            if tc.has_terminated() {
                return Err("Execution terminated".into());
            }
            let msg = msg.or_else(|| {
                tc.exception().map(|e| {
                    e.to_rust_string_lossy(tc)
                        .trim_start_matches("Error: ")
                        .to_owned()
                })
            });
            return Err(msg.unwrap_or_else(|| "The object could not be cloned.".into()));
        }
        ser.release()
    };
    let mut buffers = Vec::with_capacity(transfer.len());
    for ab in transfer {
        let v: v8::Local<v8::Value> = (*ab).into();
        buffers.push(read_bytes(tc, v).unwrap_or_default());
        ab.detach(None);
    }
    Ok(Serialized { data, buffers })
}

/// Deserialize in the current context; host objects come from `hosts`.
pub(super) fn deserialize_in<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    state: &SharedState,
    realm: RealmId,
    s: &Serialized,
    hosts: &[(InterfaceId, HostRef)],
) -> Result<v8::Local<'s, v8::Value>, String> {
    let de = v8::ValueDeserializer::new(
        scope,
        Box::new(DeDelegate {
            state: state.clone(),
            realm,
            hosts: hosts.to_vec(),
        }),
        &s.data,
    );
    let context = scope.get_current_context();
    if de.read_header(context) != Some(true) {
        return Err("Unable to deserialize cloned data.".into());
    }
    for (i, bytes) in s.buffers.iter().enumerate() {
        let store = v8::ArrayBuffer::new_backing_store_from_vec(bytes.clone()).make_shared();
        let ab = v8::ArrayBuffer::with_backing_store(scope, &store);
        de.transfer_array_buffer(i as u32, ab);
    }
    de.read_value(context)
        .ok_or_else(|| "Unable to deserialize cloned data.".into())
}

/// Serialize a held value (see [`serialize_in`]); `transfer` are held
/// `ArrayBuffer`s.
pub(super) fn serialize_handle(
    scope: &mut v8::PinScope<'_, '_>,
    state: &SharedState,
    value: crate::Handle,
    transfer: &[crate::Handle],
    host: HostWriter<'_>,
) -> Result<Serialized, String> {
    let v = load_handle(scope, state, value).map_err(|e| e.message)?;
    let mut abs = Vec::with_capacity(transfer.len());
    for h in transfer {
        let t = load_handle(scope, state, *h).map_err(|e| e.message)?;
        abs.push(
            v8::Local::<v8::ArrayBuffer>::try_from(t)
                .map_err(|_| "Value does not have a transferable type.".to_owned())?,
        );
    }
    serialize_in(scope, v, &abs, host)
}

/// Deserialize into a new handle of `realm`.
pub(super) fn deserialize_handle(
    scope: &mut v8::PinScope<'_, '_>,
    state: &SharedState,
    realm: RealmId,
    s: &Serialized,
    hosts: &[(InterfaceId, HostRef)],
) -> Result<crate::Handle, String> {
    v8::tc_scope!(let tc, scope);
    let v = deserialize_in(tc, state, realm, s, hosts)?;
    Ok(store_handle(tc, state, realm, v))
}
