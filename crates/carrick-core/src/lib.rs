//! Carrick: send files from one computer to another, wherever they are.
//!
//! Direct when the network allows it, through a relay when it does not,
//! encrypted end to end, verified piece by piece, and able to carry on by
//! itself after a dropped connection or a restart.

pub mod access;
pub mod events;
pub mod gate;
pub mod home;
pub mod manifest;
pub mod net;
pub mod proto;
pub mod recv;
pub mod send;
pub mod system;

pub use tokio_util::sync::CancellationToken;
