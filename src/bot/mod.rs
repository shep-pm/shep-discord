//! The bot half of this dog: slash commands, the embed they answer with,
//! and (once the gateway comes up in a later task) the dispatch that
//! drives both from a live connection.
//!
//! Declarations only for now. [`embed`] renders one sheep as a
//! [`serenity`] embed with its action buttons; nothing here opens a
//! gateway connection yet, and [`crate::stream`] is what already reaches
//! Discord, over REST alone, for the log-streaming half of this dog.

pub mod command;
pub mod embed;
