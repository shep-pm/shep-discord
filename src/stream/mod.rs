//! Turning the log firehose into Discord messages.
//!
//! Two stages, one module each, none of which touches a socket or the
//! network: [`buffer`] coalesces raw lines into groups under a bounded
//! queue, and [`pack`] turns a group into the chunks and messages
//! Discord's own limits allow. Splitting the stages this way is what makes
//! each one testable without a running bot: a group or a chunk is a plain
//! value, built and inspected in a test with no client, no socket, and no
//! async runtime.

pub mod buffer;
pub mod pack;
