//! `tinybrowser`: the smallest headless browser for AI agents.
//!
//! One executable. No separately shipped helper. Engine is Blitz
//! parse/style/layout/paint plus `QuickJS` behind a value-only browser
//! interface.
//! The same executable may self-spawn a profile daemon and renderer
//! processes.
//! The embeddable surface lives here; CDP is a peer crate.
//!
//! # Seam map
//!
//! ```text
//! main → {browser, cdp, webdriver}
//!        cdp → {browser, server}
//!        webdriver → {browser, server}
//!        browser → {dial, net, renderer}
//! ```

pub use browser::{
    AgentOptions, Browser, BrowserError, BrowserHandle, BrowserOpenError, BrowserOptions, Profile,
    ProfileError, ProfileName, RemoteValue, ScriptFailure, TabError, TabEvent, TabHandle, TabId,
};
