mod actor;
mod assignment;
mod broadcast;
mod browser;
pub mod child;
mod context;
mod exchange;
mod frames;
mod history;
mod link;
mod manager;
mod network;
mod profile;
mod site;
mod storage;
mod wire;

pub use actor::{
    ExecuteScriptOptions, NavigationFailure, RunUntilJsTrueOptions, TabEvent, TabHandle, TabId,
};
pub use browser::{Browser, BrowserError, BrowserHandle, BrowserOpenError, BrowserOptions};
pub use history::{EntryId, HistoryEntry, HistorySnapshot};
pub use net::{
    Agent, AgentOptions, Brand, CookieRecord, CookieSameSite, Identity, InvalidCa, InvalidIdentity,
    InvalidProxy, InvalidResolveRule, Proxy, ResolveRule, TrustedCa, UserAgent, ValidIdentity,
};
pub use profile::{Profile, ProfileError, ProfileName, default_data_home};
pub use renderer::ConsoleLevel;
pub use renderer::{
    AxNode, AxProperty, AxPropertyValue, DEFAULT_VIEWPORT, DomBoxModel, DomNode, DomRemoteNode,
    DomReply, DomRequest, DomStyleSheet, FrameId, RemoteValue, ResourceLimit, ScreenshotClip,
    ScreenshotFormat, ScreenshotRequest, ScriptFailure, ScriptSource, TabError,
};

pub use dial::SEC_CH_PREFERS_COLOR_SCHEME;
