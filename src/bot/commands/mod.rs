//! Every concrete slash command this dog answers.
//!
//! [`crate::bot::command`] (singular) is the contract every command in
//! here implements and the plumbing that registers and looks one up;
//! this module (plural) is where the commands themselves live, one file
//! each. [`crate::bot::command::registry`] is what lists them for
//! [`crate::bot::interaction::Handler`] and for Discord.

pub mod monitor;
pub mod shep;
pub mod system;
