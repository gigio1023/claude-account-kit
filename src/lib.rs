//! Claude Account Kit library crate.
//!
//! The binary is intentionally thin. Most behavior lives in this library so
//! command routing, state transitions, Keychain access, and statusline rendering
//! can be tested as separate units instead of being coupled to `main()`.

pub mod cli;

mod app;
mod claude;
mod context;
mod domain;
mod credentials;
mod desktop;
mod notification;
mod routing;
mod settings;
mod shell;
mod statusline;
mod storage;
mod switcher;
mod time;
mod util;
