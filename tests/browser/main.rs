//! One integration binary for every browser-level suite: `cargo test
//! --test browser cdp::` filters inside it, and helpers in `fixture` are
//! dead-code checked against the whole tree instead of per binary.

mod canvas;
mod cdp;
mod cookies;
mod crossorigin;
mod csp;
mod daemon;
mod fixture;
mod fonts;
mod forms;
mod history;
mod images;
mod modes;
mod pseudo_attributes;
mod renderer_process;
mod secure_context;
mod webdriver;
mod window;
