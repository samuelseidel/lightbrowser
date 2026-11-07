//! Dynamic code generation gates (Content Security Policy, ADR 0036):
//! `eval`, `Function`, and WebAssembly compilation of a realm, as Blink
//! drives them. A realm with a [`CodeGenPolicy`] has V8's "code generation
//! from strings" turned off with Blink's `EvalError` text; V8 then asks
//! [`modify_codegen`] for every string, and the policy's `check` decides
//! (report-only policies report and allow). WebAssembly goes through
//! [`allow_wasm`] the same way. Realms without a policy are untouched.

use std::pin::pin;

use super::*;

/// What is being compiled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodeGenKind {
    /// `eval`, `Function`, `setTimeout` with a string.
    Eval,
    /// `WebAssembly.compile`, `new WebAssembly.Module`, ...
    Wasm,
}

/// One compilation request a [`CodeGenPolicy`] decides.
pub struct CodeGenRequest<'a> {
    pub kind: CodeGenKind,
    /// The source string (empty for WebAssembly).
    pub source: &'a str,
    /// The script position on top of the stack: URL, 1-based line and
    /// column.
    pub location: Option<(String, u32, u32)>,
}

/// The code generation rules of a realm.
pub struct CodeGenPolicy {
    /// The `EvalError` message while strings may not become code (`None`:
    /// strings compile without asking).
    pub eval_error: Option<String>,
    /// The WebAssembly `CompileError` message (`None`: WebAssembly
    /// compiles without asking).
    pub wasm_error: Option<String>,
    /// Decides one request; true lets it compile.
    pub check: Rc<dyn Fn(CodeGenRequest<'_>) -> bool>,
}

impl std::fmt::Debug for CodeGenPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CodeGenPolicy")
            .field("eval_error", &self.eval_error)
            .field("wasm_error", &self.wasm_error)
            .finish()
    }
}

/// Install the isolate callbacks (once per engine).
pub(super) fn install(iso: &mut v8::Isolate) {
    iso.set_modify_code_generation_from_strings_callback(modify_codegen);
    iso.set_allow_wasm_code_generation_callback(allow_wasm);
}

/// The policy of the realm whose context is `ctx`.
fn policy_of(
    scope: &v8::PinScope<'_, '_>,
    ctx: v8::Local<'_, v8::Context>,
) -> Option<Rc<CodeGenPolicy>> {
    let realm = realm_of_context(scope, ctx)?;
    let state = scope.get_slot::<SharedState>()?.clone();
    let st = state.try_borrow().ok()?;
    st.realms.get(&realm)?.codegen.clone()
}

/// The script position on top of the stack (Blink's
/// `SourceLocation::Capture`).
pub(super) fn script_location(scope: &v8::PinScope<'_, '_>) -> Option<(String, u32, u32)> {
    let trace = v8::StackTrace::current_stack_trace(scope, 1)?;
    let frame = trace.get_frame(scope, 0)?;
    let url = frame
        .get_script_name_or_source_url(scope)
        .map(|s| s.to_rust_string_lossy(scope))
        .unwrap_or_default();
    Some((
        url,
        frame.get_line_number() as u32,
        frame.get_column() as u32,
    ))
}

fn modify_codegen<'s>(
    scope: &mut v8::PinScope<'s, '_>,
    source: v8::Local<'s, v8::Value>,
    _is_code_like: bool,
) -> v8::ModifyCodeGenerationFromStringsResult<'s> {
    let allowed = v8::ModifyCodeGenerationFromStringsResult {
        codegen_allowed: true,
        modified_source: None,
    };
    // Non-strings are returned unchanged by eval (Blink: no check).
    if !source.is_string() {
        return allowed;
    }
    let ctx = scope.get_current_context();
    let Some(policy) = policy_of(scope, ctx) else {
        return allowed;
    };
    if policy.eval_error.is_none() {
        return allowed;
    }
    let text = source.to_rust_string_lossy(scope);
    let location = script_location(scope);
    let ok = (policy.check)(CodeGenRequest {
        kind: CodeGenKind::Eval,
        source: &text,
        location,
    });
    v8::ModifyCodeGenerationFromStringsResult {
        codegen_allowed: ok,
        modified_source: None,
    }
}

unsafe extern "C" fn allow_wasm(
    ctx: v8::Local<v8::Context>,
    _source: v8::Local<v8::String>,
) -> bool {
    // SAFETY: V8 calls this on its thread with `ctx` entered; a callback
    // scope opens no handle scope of its own.
    let scope = pin!(unsafe { v8::CallbackScope::new(ctx) });
    let scope = scope.init();
    let Some(policy) = policy_of(&scope, ctx) else {
        return true;
    };
    if policy.wasm_error.is_none() {
        return true;
    }
    let location = script_location(&scope);
    (policy.check)(CodeGenRequest {
        kind: CodeGenKind::Wasm,
        source: "",
        location,
    })
}

/// Set the realm's gates (`scope` is in the realm's context).
pub(super) fn set(
    scope: &mut v8::PinScope<'_, '_>,
    state: &SharedState,
    realm: RealmId,
    policy: Option<CodeGenPolicy>,
) -> Result<(), JsError> {
    let ctx = scope.get_current_context();
    let eval_error = policy.as_ref().and_then(|p| p.eval_error.clone());
    let wasm_error = policy.as_ref().and_then(|p| p.wasm_error.clone());
    ctx.set_allow_generation_from_strings(eval_error.is_none());
    if let Some(m) = eval_error
        && let Some(s) = v8::String::new(scope, &m)
    {
        ffi::set_eval_error(&ctx, &s);
    }
    if let Some(m) = wasm_error
        && let Some(s) = v8::String::new(scope, &m)
    {
        ffi::set_wasm_error(&ctx, &s);
    }
    let mut st = state.borrow_mut();
    if let Some(r) = st.realms.get_mut(&realm) {
        r.codegen = policy.map(Rc::new);
    }
    Ok(())
}

/// `v8::Context::SetErrorMessageForCodeGenerationFromStrings` and
/// `SetErrorMessageForWasmCodeGeneration`, which rusty_v8 does not wrap.
/// Both are exported from the static V8 library; they are non-virtual
/// member functions taking `this` and a `Local<String>` (one pointer,
/// trivially copyable, so passed in a register under the Itanium and
/// AAPCS64 ABIs). On Windows (MSVC mangling) they are not called and V8's
/// default texts stay.
mod ffi {
    #[cfg(not(windows))]
    unsafe extern "C" {
        #[link_name = "_ZN2v87Context43SetErrorMessageForCodeGenerationFromStringsENS_5LocalINS_6StringEEE"]
        fn v8_set_eval_error(this: *const v8::Context, message: *const v8::String);
        #[link_name = "_ZN2v87Context36SetErrorMessageForWasmCodeGenerationENS_5LocalINS_6StringEEE"]
        fn v8_set_wasm_error(this: *const v8::Context, message: *const v8::String);
    }

    pub(super) fn set_eval_error(ctx: &v8::Context, message: &v8::String) {
        // SAFETY: both pointers are live handles (a `Local` is the address
        // of its handle slot, which is what the C++ `this` and `Local`
        // carry); the call only stores the string in the native context.
        #[cfg(not(windows))]
        unsafe {
            v8_set_eval_error(ctx, message)
        };
        #[cfg(windows)]
        let _ = (ctx, message);
    }

    pub(super) fn set_wasm_error(ctx: &v8::Context, message: &v8::String) {
        // SAFETY: as in `set_eval_error`.
        #[cfg(not(windows))]
        unsafe {
            v8_set_wasm_error(ctx, message)
        };
        #[cfg(windows)]
        let _ = (ctx, message);
    }
}
