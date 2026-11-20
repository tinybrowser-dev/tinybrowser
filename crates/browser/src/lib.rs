mod actor;
mod assignment;
mod broadcast;
mod browser;
pub mod child;
mod context;
mod exchange;
mod history;
mod link;
mod manager;
mod network;
mod profile;
mod site;
mod storage;
mod wire;

pub use actor::{ExecuteScriptOptions, RunUntilJsTrueOptions, TabEvent, TabHandle, TabId};
pub use browser::{Browser, BrowserError, BrowserHandle, BrowserOpenError, BrowserOptions};
pub use history::{EntryId, HistoryEntry, HistorySnapshot};
pub use net::{
    Agent, AgentOptions, CookieRecord, CookieSameSite, InvalidCa, InvalidProxy, InvalidResolveRule,
    Proxy, ResolveRule, TrustedCa, UserAgent,
};
pub use profile::{Profile, ProfileError, ProfileName, default_data_home};
pub use renderer::{
    DEFAULT_VIEWPORT, DomBoxModel, DomNode, DomRemoteNode, DomReply, DomRequest, FrameId,
    RemoteValue, ResourceLimit, ScreenshotClip, ScreenshotRequest, ScriptFailure, ScriptSource,
    TabError,
};

pub use dial::SEC_CH_PREFERS_COLOR_SCHEME;
