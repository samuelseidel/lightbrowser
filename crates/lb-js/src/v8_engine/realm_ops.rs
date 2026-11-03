//! [`RealmOps`]: the realm-building subset of [`Engine`], also usable from
//! inside a host callback through [`CallCtx`].
//!
//! A host function cannot reach the [`Engine`] (its isolate is borrowed
//! by the running callback), yet some DOM operations must build a realm on
//! the spot: Chrome creates an iframe's `about:blank` document while
//! `appendChild` runs, and `iframe.contentWindow` is readable right after.
//! `CallCtx` implements the same operations on a nested handle scope of
//! the running callback, so realm setup code written against
//! `&mut dyn RealmOps` runs both from the event loop and from a callback.

use super::*;

/// Realm creation and the value operations realm setup needs. Implemented
/// by [`Engine`] and by [`CallCtx`] (nested inside a running callback).
/// Exceptions are caught and returned as [`JsError`]s; inside a callback a
/// termination keeps unwinding.
pub trait RealmOps {
    /// Create a realm (V8 context), see [`Engine::new_realm`].
    fn new_realm(&mut self, options: RealmOptions) -> Result<RealmId, JsError>;
    /// Whether `realm` was deserialized from the startup snapshot context
    /// (its init script already ran; see [`Engine::use_snapshot_context`]).
    fn is_snapshot_realm(&self, realm: RealmId) -> bool {
        let _ = realm;
        false
    }
    /// Drop a realm, its wrappers, handles and resolvers.
    fn drop_realm(&mut self, realm: RealmId);
    fn evaluate(
        &mut self,
        realm: RealmId,
        source: &str,
        origin: ScriptOrigin<'_>,
    ) -> Result<JsValue, JsError>;
    fn evaluate_handle(
        &mut self,
        realm: RealmId,
        source: &str,
        origin: ScriptOrigin<'_>,
    ) -> Result<Handle, JsError>;
    fn call(
        &mut self,
        realm: RealmId,
        func: Handle,
        this: Arg,
        args: &[Arg],
    ) -> Result<JsValue, JsError>;
    fn call_handle(
        &mut self,
        realm: RealmId,
        func: Handle,
        this: Arg,
        args: &[Arg],
    ) -> Result<Handle, JsError>;
    /// The realm's global object.
    fn global(&mut self, realm: RealmId) -> Result<Handle, JsError>;
    fn get_property(
        &mut self,
        realm: RealmId,
        object: Handle,
        key: &str,
    ) -> Result<Handle, JsError>;
    fn set_property(
        &mut self,
        realm: RealmId,
        object: Handle,
        key: &str,
        value: Arg,
    ) -> Result<(), JsError>;
    fn make_handle(&mut self, realm: RealmId, value: Arg) -> Result<Handle, JsError>;
    /// The wrapper of a host object in `realm`, created on first use.
    fn wrap(
        &mut self,
        realm: RealmId,
        iface: InterfaceId,
        host: HostRef,
    ) -> Result<Handle, JsError>;
    fn release(&mut self, handle: Handle);
    fn new_function(
        &mut self,
        realm: RealmId,
        name: &str,
        length: u32,
        f: HostFn,
    ) -> Result<Handle, JsError>;
    fn new_constructible_function(
        &mut self,
        realm: RealmId,
        name: &str,
        length: u32,
        f: HostFn,
    ) -> Result<Handle, JsError>;
    /// See [`Engine::set_time_zone`].
    fn set_time_zone(&mut self, time_zone: &str) -> bool;
    /// See [`Engine::set_default_locale`].
    fn set_default_locale(&mut self, locale: &str);
    /// See [`Engine::heap_stats`].
    fn heap_stats(&mut self) -> HeapStats;
    /// Set the realm's security token. Realms with equal tokens can reach
    /// each other's globals (same-origin frames); V8 denies access
    /// between globals whose tokens differ (`TypeError: no access`).
    fn set_security_token(&mut self, realm: RealmId, token: &str) -> Result<(), JsError>;
    /// Delete the configurable own string-keyed properties of the realm's
    /// global whose names `keep` rejects (engine built-ins a browser does
    /// not expose), as a script doing `delete globalThis[name]` would.
    /// Returns how many own string keys the global has afterwards. Reads
    /// no property values, so lazy interface objects stay lazy.
    fn delete_globals_except(
        &mut self,
        realm: RealmId,
        keep: &dyn Fn(&str) -> bool,
    ) -> Result<usize, JsError>;
}

/// Shared body of [`RealmOps::delete_globals_except`].
fn delete_globals(
    scope: &mut v8::PinScope<'_, '_>,
    keep: &dyn Fn(&str) -> bool,
) -> Result<usize, JsError> {
    let global = scope.get_current_context().global(scope);
    let args = v8::GetPropertyNamesArgs {
        mode: v8::KeyCollectionMode::OwnOnly,
        property_filter: v8::PropertyFilter::ALL_PROPERTIES | v8::PropertyFilter::SKIP_SYMBOLS,
        index_filter: v8::IndexFilter::IncludeIndices,
        key_conversion: v8::KeyConversionMode::ConvertToString,
    };
    let names = global
        .get_own_property_names(scope, args)
        .ok_or_else(|| JsError::host("global keys"))?;
    let n = names.length();
    let k_conf = v8::String::new(scope, "configurable")
        .ok_or_else(|| JsError::host("string"))?
        .into();
    let mut left = n as usize;
    for i in 0..n {
        let Some(key) = names.get_index(scope, i) else {
            continue;
        };
        if keep(&key.to_rust_string_lossy(scope)) {
            continue;
        }
        let Ok(name) = v8::Local::<v8::Name>::try_from(key) else {
            continue;
        };
        let configurable = global
            .get_own_property_descriptor(scope, name)
            .and_then(|d| v8::Local::<v8::Object>::try_from(d).ok())
            .is_some_and(|d| d.get(scope, k_conf).is_some_and(|v| v.is_true()));
        if configurable && global.delete(scope, key) == Some(true) {
            left -= 1;
        }
    }
    Ok(left)
}

/// Shared body of [`RealmOps::set_security_token`]: the token is an
/// internalized string, so equal text gives the identical token object.
fn set_token(scope: &mut v8::PinScope<'_, '_>, token: &str) -> Result<(), JsError> {
    let t = v8::String::new_from_utf8(scope, token.as_bytes(), v8::NewStringType::Internalized)
        .ok_or_else(|| JsError::host("token too long"))?;
    scope.get_current_context().set_security_token(t.into());
    Ok(())
}

impl RealmOps for Engine {
    fn new_realm(&mut self, options: RealmOptions) -> Result<RealmId, JsError> {
        Engine::new_realm(self, options)
    }
    fn is_snapshot_realm(&self, realm: RealmId) -> bool {
        is_snapshot_realm(&self.state, realm)
    }
    fn drop_realm(&mut self, realm: RealmId) {
        Engine::drop_realm(self, realm)
    }
    fn evaluate(
        &mut self,
        realm: RealmId,
        source: &str,
        origin: ScriptOrigin<'_>,
    ) -> Result<JsValue, JsError> {
        Engine::evaluate(self, realm, source, origin)
    }
    fn evaluate_handle(
        &mut self,
        realm: RealmId,
        source: &str,
        origin: ScriptOrigin<'_>,
    ) -> Result<Handle, JsError> {
        Engine::evaluate_handle(self, realm, source, origin)
    }
    fn call(
        &mut self,
        realm: RealmId,
        func: Handle,
        this: Arg,
        args: &[Arg],
    ) -> Result<JsValue, JsError> {
        Engine::call(self, realm, func, this, args)
    }
    fn call_handle(
        &mut self,
        realm: RealmId,
        func: Handle,
        this: Arg,
        args: &[Arg],
    ) -> Result<Handle, JsError> {
        Engine::call_handle(self, realm, func, this, args)
    }
    fn global(&mut self, realm: RealmId) -> Result<Handle, JsError> {
        Engine::global(self, realm)
    }
    fn get_property(
        &mut self,
        realm: RealmId,
        object: Handle,
        key: &str,
    ) -> Result<Handle, JsError> {
        Engine::get_property(self, realm, object, key)
    }
    fn set_property(
        &mut self,
        realm: RealmId,
        object: Handle,
        key: &str,
        value: Arg,
    ) -> Result<(), JsError> {
        Engine::set_property(self, realm, object, key, value)
    }
    fn make_handle(&mut self, realm: RealmId, value: Arg) -> Result<Handle, JsError> {
        Engine::make_handle(self, realm, value)
    }
    fn wrap(
        &mut self,
        realm: RealmId,
        iface: InterfaceId,
        host: HostRef,
    ) -> Result<Handle, JsError> {
        Engine::wrap(self, realm, iface, host)
    }
    fn release(&mut self, handle: Handle) {
        Engine::release(self, handle)
    }
    fn new_function(
        &mut self,
        realm: RealmId,
        name: &str,
        length: u32,
        f: HostFn,
    ) -> Result<Handle, JsError> {
        Engine::new_function(self, realm, name, length, f)
    }
    fn new_constructible_function(
        &mut self,
        realm: RealmId,
        name: &str,
        length: u32,
        f: HostFn,
    ) -> Result<Handle, JsError> {
        Engine::new_constructible_function(self, realm, name, length, f)
    }
    fn set_time_zone(&mut self, time_zone: &str) -> bool {
        Engine::set_time_zone(self, time_zone)
    }
    fn set_default_locale(&mut self, locale: &str) {
        Engine::set_default_locale(self, locale)
    }
    fn heap_stats(&mut self) -> HeapStats {
        Engine::heap_stats(self)
    }
    fn set_security_token(&mut self, realm: RealmId, token: &str) -> Result<(), JsError> {
        self.run(realm, |scope, _| set_token(scope, token))
    }
    fn delete_globals_except(
        &mut self,
        realm: RealmId,
        keep: &dyn Fn(&str) -> bool,
    ) -> Result<usize, JsError> {
        self.run(realm, |scope, _| delete_globals(scope, keep))
    }
}

impl CallCtx<'_, '_, '_> {
    /// Run `f` in `realm`'s context on a nested handle scope of this
    /// callback.
    fn run_in<R>(
        &mut self,
        realm: RealmId,
        f: impl for<'a, 'b> FnOnce(&mut v8::PinScope<'a, 'b>, &SharedState) -> Result<R, JsError>,
    ) -> Result<R, JsError> {
        let state = self.state.clone();
        v8::scope!(let hs, self.scope);
        let context = {
            let st = state.borrow();
            let entry = st
                .realms
                .get(&realm)
                .ok_or_else(|| JsError::host(format!("unknown realm {realm:?}")))?;
            v8::Local::new(hs, &entry.context)
        };
        let cs = &mut v8::ContextScope::new(hs, context);
        f(cs, &state)
    }

    fn function_in(
        &mut self,
        realm: RealmId,
        name: &str,
        length: u32,
        f: HostFn,
        behavior: v8::ConstructorBehavior,
    ) -> Result<Handle, JsError> {
        let idx = self.state.borrow_mut().push_callback(Callback::Fn(f));
        self.run_in(realm, |scope, state| {
            let data = v8::Integer::new_from_unsigned(scope, idx);
            let func = v8::Function::builder(host_trampoline)
                .data(data.into())
                .length(length as i32)
                .constructor_behavior(behavior)
                .build(scope)
                .ok_or_else(|| JsError::host("function creation failed"))?;
            let n = v8_str(scope, name);
            func.set_name(n);
            Ok(store_handle(scope, state, realm, func.into()))
        })
    }

    /// The realm whose script started the current call chain (V8's entered
    /// context): the caller when a page calls into another frame's
    /// functions. Used for cross-origin checks.
    pub fn entered_realm(&mut self) -> RealmId {
        let ctx = self.scope.get_entered_or_microtask_context();
        realm_of_context(self.scope, ctx).unwrap_or(self.realm)
    }
}

impl RealmOps for CallCtx<'_, '_, '_> {
    fn new_realm(&mut self, options: RealmOptions) -> Result<RealmId, JsError> {
        let state = self.state.clone();
        v8::scope!(let hs, self.scope);
        new_realm_in(hs, &state, options)
    }
    fn is_snapshot_realm(&self, realm: RealmId) -> bool {
        is_snapshot_realm(&self.state, realm)
    }
    fn drop_realm(&mut self, realm: RealmId) {
        drop_realm_state(self.scope, &self.state, realm);
    }
    fn evaluate(
        &mut self,
        realm: RealmId,
        source: &str,
        origin: ScriptOrigin<'_>,
    ) -> Result<JsValue, JsError> {
        self.run_in(realm, |scope, _| {
            v8::tc_scope!(let tc, scope);
            match run_script(tc, source, origin) {
                Some(v) => Ok(to_js_value(tc, v)),
                None => Err(caught_nested(tc)),
            }
        })
    }
    fn evaluate_handle(
        &mut self,
        realm: RealmId,
        source: &str,
        origin: ScriptOrigin<'_>,
    ) -> Result<Handle, JsError> {
        self.run_in(realm, |scope, state| {
            v8::tc_scope!(let tc, scope);
            match run_script(tc, source, origin) {
                Some(v) => Ok(store_handle(tc, state, realm, v)),
                None => Err(caught_nested(tc)),
            }
        })
    }
    fn call(
        &mut self,
        realm: RealmId,
        func: Handle,
        this: Arg,
        args: &[Arg],
    ) -> Result<JsValue, JsError> {
        self.run_in(realm, |scope, state| {
            v8::tc_scope!(let tc, scope);
            match call_inner(tc, state, realm, func, &this, args)? {
                Some(v) => Ok(to_js_value(tc, v)),
                None => Err(caught_nested(tc)),
            }
        })
    }
    fn call_handle(
        &mut self,
        realm: RealmId,
        func: Handle,
        this: Arg,
        args: &[Arg],
    ) -> Result<Handle, JsError> {
        self.run_in(realm, |scope, state| {
            v8::tc_scope!(let tc, scope);
            match call_inner(tc, state, realm, func, &this, args)? {
                Some(v) => Ok(store_handle(tc, state, realm, v)),
                None => Err(caught_nested(tc)),
            }
        })
    }
    fn global(&mut self, realm: RealmId) -> Result<Handle, JsError> {
        self.run_in(realm, |scope, state| {
            let g = scope.get_current_context().global(scope);
            Ok(store_handle(scope, state, realm, g.into()))
        })
    }
    fn get_property(
        &mut self,
        realm: RealmId,
        object: Handle,
        key: &str,
    ) -> Result<Handle, JsError> {
        self.run_in(realm, |scope, state| {
            v8::tc_scope!(let tc, scope);
            let v = load_handle(tc, state, object)?;
            let obj = v.to_object(tc).ok_or_else(|| caught_nested(tc))?;
            let k = v8_str(tc, key);
            match obj.get(tc, k.into()) {
                Some(v) => Ok(store_handle(tc, state, realm, v)),
                None => Err(caught_nested(tc)),
            }
        })
    }
    fn set_property(
        &mut self,
        realm: RealmId,
        object: Handle,
        key: &str,
        value: Arg,
    ) -> Result<(), JsError> {
        self.run_in(realm, |scope, state| {
            v8::tc_scope!(let tc, scope);
            let v = load_handle(tc, state, object)?;
            if let Ok(o) = v8::Local::<v8::Object>::try_from(v) {
                let val = match arg_to_v8(tc, state, realm, &value) {
                    Ok(v) => v,
                    Err(_) => return Err(caught_nested(tc)),
                };
                let k = v8_str(tc, key);
                if o.set(tc, k.into(), val).is_none() {
                    return Err(caught_nested(tc));
                }
            }
            Ok(())
        })
    }
    fn make_handle(&mut self, realm: RealmId, value: Arg) -> Result<Handle, JsError> {
        self.run_in(realm, |scope, state| {
            v8::tc_scope!(let tc, scope);
            match arg_to_v8(tc, state, realm, &value) {
                Ok(v) => Ok(store_handle(tc, state, realm, v)),
                Err(_) => Err(caught_nested(tc)),
            }
        })
    }
    fn wrap(
        &mut self,
        realm: RealmId,
        iface: InterfaceId,
        host: HostRef,
    ) -> Result<Handle, JsError> {
        self.run_in(realm, |scope, state| {
            let obj = wrap_host(scope, state, realm, iface, host)?;
            Ok(store_handle(scope, state, realm, obj.into()))
        })
    }
    fn release(&mut self, handle: Handle) {
        self.state
            .borrow_mut()
            .handles
            .remove(handle.idx, handle.generation);
    }
    fn new_function(
        &mut self,
        realm: RealmId,
        name: &str,
        length: u32,
        f: HostFn,
    ) -> Result<Handle, JsError> {
        self.function_in(realm, name, length, f, v8::ConstructorBehavior::Throw)
    }
    fn new_constructible_function(
        &mut self,
        realm: RealmId,
        name: &str,
        length: u32,
        f: HostFn,
    ) -> Result<Handle, JsError> {
        self.function_in(realm, name, length, f, v8::ConstructorBehavior::Allow)
    }
    fn set_time_zone(&mut self, time_zone: &str) -> bool {
        if !set_icu_time_zone(time_zone) {
            return false;
        }
        self.scope
            .date_time_configuration_change_notification(v8::TimeZoneDetection::Skip);
        true
    }
    fn set_default_locale(&mut self, locale: &str) {
        if !locale.is_empty() && !locale.contains('\0') && v8::icu::get_language_tag() != locale {
            v8::icu::set_default_locale(&locale.replace('-', "_"));
        }
    }
    fn heap_stats(&mut self) -> HeapStats {
        let limit_hits = self
            .state
            .borrow()
            .heap
            .as_ref()
            .map_or(0, |h| h.hits.get());
        let s = self.scope.get_heap_statistics();
        HeapStats {
            used: s.used_heap_size(),
            total: s.total_heap_size(),
            limit: s.heap_size_limit(),
            limit_hits,
        }
    }
    fn set_security_token(&mut self, realm: RealmId, token: &str) -> Result<(), JsError> {
        self.run_in(realm, |scope, _| set_token(scope, token))
    }
    fn delete_globals_except(
        &mut self,
        realm: RealmId,
        keep: &dyn Fn(&str) -> bool,
    ) -> Result<usize, JsError> {
        self.run_in(realm, |scope, _| delete_globals(scope, keep))
    }
}
