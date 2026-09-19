//! Every concrete slash command this dog answers.
//!
//! [`crate::bot::command`] (singular) is the contract every command in
//! here implements and the plumbing that registers and looks one up;
//! this module (plural) is where the commands themselves live, one file
//! each. [`crate::bot::command::registry`] is what lists them for
//! [`crate::bot::interaction::Handler`] and for Discord.

use serenity::all::{CommandOptionType, CreateCommandOption};

pub mod monitor;
pub mod shep;
pub mod system;

/// One subcommand that takes no options at all.
///
/// Here rather than in either command file because both of them declare
/// subcommands of this shape and the two copies were byte for byte the
/// same: `/shep list` and `/shep save`, and all three of `/monitor`'s.
/// A child module reads an ancestor's private items, so neither caller
/// needs anything exported.
fn bare_subcommand(name: &str, description: &str) -> CreateCommandOption {
    CreateCommandOption::new(CommandOptionType::SubCommand, name, description)
}
