//! The Carrick window: send and receive with a few clicks.
//!
//! It is a face over `carrick-core`, like the terminal: it calls the same
//! functions and shows the events they report. It does nothing the terminal
//! cannot do.

pub mod app;
pub mod fmt;
pub mod job;
mod look;

pub use app::{App, Screen, Settings};
pub use look::install;
