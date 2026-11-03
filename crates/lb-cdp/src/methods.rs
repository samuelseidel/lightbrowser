//! The CDP surface LightBrowser answers. `/json/protocol` is built from
//! these lists; a test checks that every listed command is dispatched.

/// Commands by domain.
pub const COMMANDS: &[(&str, &[&str])] = &[
    (
        "Browser",
        &[
            "getVersion",
            "close",
            "setDownloadBehavior",
            "getWindowForTarget",
        ],
    ),
    (
        "Target",
        &[
            "setAutoAttach",
            "createBrowserContext",
            "disposeBrowserContext",
            "getBrowserContexts",
            "createTarget",
            "attachToTarget",
            "attachToBrowserTarget",
            "detachFromTarget",
            "closeTarget",
            "activateTarget",
            "getTargets",
            "getTargetInfo",
            "setDiscoverTargets",
        ],
    ),
    (
        "Page",
        &[
            "enable",
            "disable",
            "navigate",
            "reload",
            "stopLoading",
            "getFrameTree",
            "getNavigationHistory",
            "getLayoutMetrics",
            "createIsolatedWorld",
            "addScriptToEvaluateOnNewDocument",
            "removeScriptToEvaluateOnNewDocument",
            "setLifecycleEventsEnabled",
            "setInterceptFileChooserDialog",
            "handleJavaScriptDialog",
            "bringToFront",
            "close",
            "captureScreenshot",
        ],
    ),
    (
        "Runtime",
        &[
            "enable",
            "disable",
            "evaluate",
            "callFunctionOn",
            "getProperties",
            "releaseObject",
            "releaseObjectGroup",
            "addBinding",
            "runIfWaitingForDebugger",
        ],
    ),
    (
        "DOM",
        &[
            "enable",
            "disable",
            "getDocument",
            "describeNode",
            "resolveNode",
            "requestNode",
            "getBoxModel",
            "getContentQuads",
            "scrollIntoViewIfNeeded",
            "setFileInputFiles",
            "querySelector",
            "querySelectorAll",
            "getOuterHTML",
        ],
    ),
    (
        "Input",
        &[
            "dispatchMouseEvent",
            "dispatchKeyEvent",
            "insertText",
            "dispatchTouchEvent",
        ],
    ),
    (
        "Network",
        &[
            "enable",
            "disable",
            "setExtraHTTPHeaders",
            "getCookies",
            "getAllCookies",
            "setCookie",
            "setCookies",
            "deleteCookies",
            "clearBrowserCookies",
            "clearBrowserCache",
            "setCacheDisabled",
            "getResponseBody",
            "setUserAgentOverride",
        ],
    ),
    ("Storage", &["getCookies", "setCookies", "clearCookies"]),
    (
        "Fetch",
        &[
            "enable",
            "disable",
            "continueRequest",
            "fulfillRequest",
            "failRequest",
        ],
    ),
    (
        "Emulation",
        &[
            "setDeviceMetricsOverride",
            "clearDeviceMetricsOverride",
            "setUserAgentOverride",
            "setTimezoneOverride",
            "setLocaleOverride",
            "setFocusEmulationEnabled",
            "setEmulatedMedia",
            "setTouchEmulationEnabled",
            "setGeolocationOverride",
        ],
    ),
    (
        "Accessibility",
        &["enable", "disable", "getFullAXTree", "queryAXTree"],
    ),
    (
        "Security",
        &["enable", "disable", "setIgnoreCertificateErrors"],
    ),
    ("Log", &["enable", "disable", "clear"]),
    ("Performance", &["enable", "disable"]),
    (
        "LightBrowser",
        &[
            "setPersona",
            "fingerprintReport",
            "humanClick",
            "humanType",
            "humanScroll",
        ],
    ),
];

/// Events by domain.
pub const EVENTS: &[(&str, &[&str])] = &[
    (
        "Target",
        &[
            "attachedToTarget",
            "detachedFromTarget",
            "targetCreated",
            "targetDestroyed",
            "targetInfoChanged",
        ],
    ),
    (
        "Page",
        &[
            "lifecycleEvent",
            "frameAttached",
            "frameDetached",
            "frameNavigated",
            "navigatedWithinDocument",
            "frameStartedLoading",
            "frameStoppedLoading",
            "loadEventFired",
            "domContentEventFired",
            "javascriptDialogOpening",
        ],
    ),
    (
        "Runtime",
        &[
            "executionContextCreated",
            "executionContextDestroyed",
            "executionContextsCleared",
            "consoleAPICalled",
            "exceptionThrown",
            "bindingCalled",
        ],
    ),
    (
        "Network",
        &[
            "requestWillBeSent",
            "responseReceived",
            "loadingFinished",
            "loadingFailed",
        ],
    ),
    ("Fetch", &["requestPaused"]),
];

/// Whether `method` (`Domain.name`) is a command we answer.
pub fn is_supported(method: &str) -> bool {
    let Some((domain, name)) = method.split_once('.') else {
        return false;
    };
    COMMANDS
        .iter()
        .any(|(d, names)| *d == domain && names.contains(&name))
}

/// A minimal `/json/protocol` document: domains with command and event names.
pub fn protocol_json() -> serde_json::Value {
    let domains: Vec<serde_json::Value> = COMMANDS
        .iter()
        .map(|(domain, cmds)| {
            let events = EVENTS
                .iter()
                .find(|(d, _)| d == domain)
                .map_or(&[][..], |(_, e)| *e);
            serde_json::json!({
                "domain": domain,
                "experimental": *domain == "LightBrowser",
                "commands": cmds.iter().map(|c| serde_json::json!({"name": c})).collect::<Vec<_>>(),
                "events": events.iter().map(|e| serde_json::json!({"name": e})).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({"version": {"major": "1", "minor": "3"}, "domains": domains})
}
