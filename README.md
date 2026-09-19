# shep-discord

[![Crates.io Version](https://img.shields.io/crates/v/shep-discord.svg)](https://crates.io/crates/shep-discord)
[![License](https://img.shields.io/crates/l/shep-discord.svg)](https://github.com/shep-pm/shep-discord#license)
[![MSRV](https://img.shields.io/crates/msrv/shep-discord.svg)](https://crates.io/crates/shep-discord)
[![CI](https://github.com/shep-pm/shep-discord/actions/workflows/test.yml/badge.svg)](https://github.com/shep-pm/shep-discord/actions/workflows/test.yml)

A Discord dog for [shep](https://github.com/shep-pm/shep).

It streams a sheep's stdout and stderr into Discord channels and keeps a
live embed per sheep in a monitor channel. Three slash commands let an
operator drive the flock and check on the host without leaving Discord.

It is an external dog. Nothing here is built into shep: it is an ordinary
binary you adopt, and it talks to the daemon over the same socket the CLI
uses.

## What bark already does

shep ships `bark`, a built-in dog with a Discord webhook, a rule engine and
per-subject debounce. This dog does not repeat that.

| | bark | shep-discord |
| --- | --- | --- |
| lifecycle and threshold alerts | yes | no, deliberately |
| log line streaming | no | yes |
| slash commands, buttons, autocomplete | no | yes |
| live monitor embeds | no | yes |

bark delivers one HTTP POST per firing. A log firehose through it would
rate-limit and would evict real alerts, so log streaming and alerting stay
on separate paths.

## Install

```sh
cargo install shep-discord
shep adopt shep-discord
```

`shep adopt` records the binary in `shep.toml` and starts it. From then on
the shepherd supervises it like anything else in the flock, and `shep dogs`
lists it.

Adopting also asks the binary two questions and reads the answers off its
stdout, before it opens a socket or a file. `--version` gives the build and
the wire protocol it speaks; a dog below the shepherd's floor is refused
right there. `--schema` gives a JSON Schema for the settings below, which is
what `shep lookout` draws its config pane from.

The name it is adopted under is the name shep hands it in `$SHEP_DOG_NAME`,
and it is also the config key: `dogs.toml` reads `[discord]`. Running the
binary by hand, with no `$SHEP_DOG_NAME` set, falls back to the same
`[discord]` section, so a manual run still picks up your settings.

## Configuration

Everything lives in one table in `dogs.toml`, next to `shep.toml` in your
shep home. Ask the binary for a starting point:

```sh
shep-discord --print-config >> ~/.shep/dogs.toml
```

Every line it prints is commented, so appending it changes nothing until you
uncomment something. `PRINT_CONFIG` in `src/config.rs` is where that block
lives, and a test holds it to the values the code actually uses.

The smallest config that runs:

```toml
[discord]
token = "your-bot-token"
guild_id = 123456789012345678
```

| Option | Default | Notes |
| --- | --- | --- |
| `token` | none, required | The Discord bot token. This is a secret: `--schema` marks it, and `shep lookout`'s config pane masks it. Never logged; `Config`'s own `Debug` prints `<redacted>` in its place. |
| `guild_id` | none, required | The guild (server) this bot serves. A Discord snowflake, so `0` is refused rather than a real value. |
| `monitor_channel` | unset | Channel for the live monitor. Unset disables it. |
| `monitor_interval` | unset | How often the monitor refreshes, e.g. `"1m"`. Unset means the monitor does not start on boot; `/monitor start` can still start it for the rest of the process's life. A value under 15 seconds is raised to that floor rather than refused. |
| `log_channel` | unset | Channel for stdout lines. Unset disables that stream. |
| `err_channel` | unset | Channel for stderr lines. Unset disables that stream. |
| `flush` | `"1s"` | How often the buffer drains. |
| `coalesce` | `"1s"` | How wide a window joins lines into one embed. |
| `buffer_lines` | `2000` | How many lines the buffer holds before dropping the oldest. `0` is refused. |
| `ignore_dogs` | `false` | Hide other dogs from listings and the monitor. |

`monitor_interval` is what starts the monitor on boot and sets its pace.
`/monitor start` starts it for the running process only, on that same
interval, and never writes to `dogs.toml`.

## Commands

All three carry the administrator permission gate.

### `/shep <verb> [name]`

Drives the flock.

| Verb | Does |
| --- | --- |
| `start`, `stop`, `restart`, `reload`, `delete` | Acts on the named sheep or selector. |
| `flush` | Empties a sheep's log files. This is the only path in the whole dog that can build a flush request; nothing else in the codebase is allowed to. |
| `list` | Lists every sheep in the flock. |
| `save` | Writes the muster roll now. |
| `reopen` | Asks the shepherd to reopen a sheep's log files. |

`name` is required for every verb except `list` and `save`, which act on the
whole flock. Autocomplete suggests names from the live flock.

### `/monitor start|update|stop`

Toggles the live monitor for this process's lifetime. It does not write
`dogs.toml`. `start` refreshes on `monitor_interval`, or on the 15 second
floor when that is unset, and refuses to run twice at once. `update`
redraws every sheep now and leaves the schedule alone. `stop` ends the
refresh task and waits for it to finish. All three say what they did.

On every boot the monitor rediscovers itself. It reads `monitor_channel` a
page at a time, keeps the messages this bot wrote, and reads each sheep id
back out of the buttons. It stops once it has found every sheep in the
flock, or once the channel runs out. That rebuilds the per-sheep cache for
free, so no state has to survive the restart.

### `/system`

Reports the shepherd's host: CPU, memory, disk throughput and network
throughput.

This is built on `Request::HostUsage`, which arrived in shep's protocol 9.
A shepherd older than that refuses the verb by name, and `/system` answers
with an error instead of an embed. Nothing else this dog does is affected.
A shepherd accepts any peer at or above its own `MIN_SUPPORTED`, and that
is still protocol 8, so `/shep` and `/monitor` work against a shepherd that
has never heard of `HostUsage`.

## Building from source

```sh
git clone https://github.com/shep-pm/shep-discord
cd shep-discord
cargo test --locked --bins --tests
```

There is no `--lib`: this crate has no library target.

## License

MIT OR Apache-2.0, at your option.
