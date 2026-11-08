//! Standards-based JMAP transport.
//!
//! The wire implementation is provided by Stalwart Labs' `jmap-client`, which
//! implements RFC 8620 (Core), RFC 8621 (Mail and Submission), RFC 8887
//! (WebSocket), and EventSource push. This module owns Flectar's discovery,
//! capability policy, durable state-token integration, local projections, and
//! pending-action semantics.

pub mod client;
pub(crate) mod held;
pub mod sync;
