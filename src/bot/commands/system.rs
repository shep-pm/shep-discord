//! `/system`: what the shepherd's own host is doing right now.
//!
//! 131 lines of `os.cpus()` arithmetic in `services/system.ts` become one
//! request, [`shep_client::shep_core::protocol::HostUsage`], and this file
//! only renders what comes back. `HostUsage` also carries disk and network
//! rates the old embed never had, so this one draws them too, whenever the
//! shepherd reports them.
//!
//! [`render`] is split out from [`System::run`] so the embed itself, this
//! module's own tests below, is testable with a bare [`HostUsage`] value
//! and no [`State`], no shepherd, and no gateway behind either.
//! [`System::run`] itself is not: it needs a live `&Context`
//! to answer with, which nothing outside serenity's own crate can build
//! (see [`crate::bot::interaction`]'s module doc), so it is exercised only
//! by hand against a real guild.

use core::future::Future;
use std::pin::Pin;

use serenity::all::{
    Colour, CommandInteraction, Context, CreateCommand, CreateEmbed,
    CreateInteractionResponseFollowup, Permissions,
};
use shep_client::shep_core::{protocol::HostUsage, values::MemSize};

use crate::{
    bot::command::{Command, State},
    error::Error,
};

/// `/system`. Answers with one embed describing the shepherd's own host:
/// CPU, memory, and, when the shepherd samples them, disk and network
/// throughput.
pub struct System;

impl Command for System {
    fn data(&self) -> CreateCommand {
        CreateCommand::new(self.name())
            .description("What the shepherd's own host is doing right now.")
            .default_member_permissions(Permissions::ADMINISTRATOR)
    }

    fn name(&self) -> &'static str {
        "system"
    }

    fn run<'a>(
        &'a self,
        ctx: &'a Context,
        interaction: &'a CommandInteraction,
        state: &'a State,
    ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + 'a>> {
        Box::pin(async move {
            let usage = state.live.host_usage().await?;
            interaction
                .create_followup(
                    ctx,
                    CreateInteractionResponseFollowup::new()
                        .embed(render(usage))
                        .ephemeral(true),
                )
                .await?;
            Ok(())
        })
    }
}

/// One rate, rendered as bytes a second in each direction.
///
/// A helper rather than writing the sentence out twice: disk and network
/// both carry a `(u64, u64)` pair on the same units, read/write for disk
/// and received/sent for network, and [`MemSize`] is what already renders
/// a byte count the way this dog's other embeds do.
fn rate_value(pair: (u64, u64), first: &str, second: &str) -> String {
    let (a, b) = pair;
    format!(
        "{first} {}/s, {second} {}/s",
        MemSize::from_bytes(a),
        MemSize::from_bytes(b)
    )
}

/// The sentence shown when the shepherd cannot sample its own host at all.
///
/// A function rather than an inline literal so the dash check can reach it
/// directly, the same reason `main`'s own person-facing messages are
/// functions.
fn not_sampling_message() -> &'static str {
    "The shepherd is not sampling host usage."
}

/// Render one `/system` answer.
///
/// `None` is a state to say in a sentence, not an error: a shepherd can
/// run with host sampling turned off, and that is not this dog's own
/// failure to report. A shepherd too old to know the `HostUsage` verb at
/// all is the other case, and it never reaches here: [`Live::host_usage`]
/// answers that with [`Error::Request`], which `System::run` propagates
/// with `?` before this function ever runs.
///
/// [`Live::host_usage`]: crate::shepherd::Live::host_usage
fn render(usage: Option<HostUsage>) -> CreateEmbed {
    let embed = CreateEmbed::new().title("System").colour(Colour::BLUE);
    let Some(usage) = usage else {
        return embed.description(not_sampling_message());
    };

    let cpu = usage
        .cpu_percent
        .map_or_else(|| "not yet sampled".to_owned(), |pct| format!("{pct:.1}%"));
    let memory = format!(
        "{} of {}",
        MemSize::from_bytes(usage.memory_used_bytes),
        MemSize::from_bytes(usage.memory_total_bytes)
    );

    let mut embed = embed.field("CPU", cpu, true).field("Memory", memory, true);

    if let Some(disk) = usage.disk_bytes_per_second {
        embed = embed.field("Disk", rate_value(disk, "read", "write"), true);
    }
    if let Some(network) = usage.network_bytes_per_second {
        embed = embed.field("Network", rate_value(network, "received", "sent"), true);
    }

    embed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json(embed: CreateEmbed) -> serde_json::Value {
        serde_json::to_value(embed).expect("json")
    }

    fn field<'a>(value: &'a serde_json::Value, name: &str) -> &'a str {
        value["fields"]
            .as_array()
            .expect("fields")
            .iter()
            .find(|field| field["name"] == name)
            .unwrap_or_else(|| panic!("no {name} field in {value}"))["value"]
            .as_str()
            .expect("value")
    }

    #[test]
    fn every_registered_field_is_admin_gated() {
        let json = serde_json::to_value(System.data()).expect("json");
        assert!(json.get("default_member_permissions").is_some());
    }

    #[test]
    fn the_command_name_matches_its_own_registration() {
        let json = serde_json::to_value(System.data()).expect("json");
        assert_eq!(json["name"], System.name());
    }

    #[test]
    fn a_shepherd_not_sampling_its_host_says_so_rather_than_drawing_zeroes() {
        let value = json(render(None));
        assert_eq!(value["description"], not_sampling_message());
        assert!(
            value.get("fields").is_none() || value["fields"].as_array().unwrap().is_empty(),
            "a shepherd with nothing to report draws no field, not zeroes: {value}"
        );
    }

    #[test]
    fn the_embed_names_every_present_field() {
        let usage = HostUsage {
            cpu_percent: Some(12.5),
            memory_used_bytes: 512 << 20,
            memory_total_bytes: 4 << 30,
            disk_bytes_per_second: Some((1 << 20, 2 << 20)),
            network_bytes_per_second: Some((3 << 10, 4 << 10)),
        };
        let value = json(render(Some(usage)));
        assert_eq!(field(&value, "CPU"), "12.5%");
        assert_eq!(field(&value, "Memory"), "512M of 4G");
        assert_eq!(field(&value, "Disk"), "read 1M/s, write 2M/s");
        assert_eq!(field(&value, "Network"), "received 3K/s, sent 4K/s");
    }

    #[test]
    fn disk_and_network_are_each_drawn_only_when_present() {
        let usage = HostUsage {
            cpu_percent: None,
            memory_used_bytes: 1 << 20,
            memory_total_bytes: 2 << 20,
            disk_bytes_per_second: None,
            network_bytes_per_second: None,
        };
        let value = json(render(Some(usage)));
        assert_eq!(field(&value, "CPU"), "not yet sampled");
        let names: Vec<&str> = value["fields"]
            .as_array()
            .expect("fields")
            .iter()
            .map(|field| field["name"].as_str().expect("name"))
            .collect();
        assert_eq!(names, vec!["CPU", "Memory"]);
    }

    #[test]
    fn nothing_printed_for_a_person_carries_a_dash() {
        crate::test_support::assert_no_dashes(not_sampling_message());
    }
}
