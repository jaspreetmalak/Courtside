//! Shared DTOs and wire protocol for Courtside.
//!
//! This crate is compiled into both the server and the native clients.
//! It must contain only data types (serde) — no business logic, no
//! configuration, and nothing secret-adjacent.

pub mod models;
pub mod protocol;

pub use models::*;
pub use protocol::*;
