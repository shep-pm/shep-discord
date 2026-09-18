# Refactor map

`shep-discord`: one binary, one dog. A serenity gateway client and a shep bus subscription in the same process, sharing one config section.

Module shape follows `shep-log-rotate`: one module owns the daemon conversation, one owns stopping, the rest are pure. The two source repos are `pm2-discord-logger` (`pdl/`) and `discord-pm2` (`dp2/`) below.

## New structure

```
src/
  main.rs
    probe / Action / Identity / connect / run
      ← was dp2/src/index.ts, pdl/src/app.ts:6
      Action: rewrite
      Notes: first line is shep_client::dogs::probe::<config::Section>, before
             the argument parser, for the same reason shep-log-rotate does it:
             `shep adopt` spawns the binary with --version then --schema and
             reads one line. Identity is copied from shep-log-rotate's
             main.rs:236 unchanged, including the rule that the handshake name
             comes from $SHEP_DOG_NAME and is never guessed. DEFAULT_NAME is
             "discord". Replaces both repos' entry points: dp2's
             required-config check moves into config.rs, pdl's io.initModule
             has no analogue at all.

  config.rs
    Section / Config / PRINT_CONFIG
      ← was dp2/config/default.json, pdl/src/types.ts:1
      Action: rewrite
      Notes: one [discord] section replaces a JSON file and nine `pm2 set`
             booleans. token carries #[shep(secret)]; Debug is hand-written
             and redacted, pinned by an exact-string test, the way bark's Sink
             is. The nine event booleans are NOT ported: bark owns event
             alerting. Fields: token, guild_id, monitor_channel, log_channel,
             err_channel, buffer, coalesce, ignore_dogs, monitor_interval.
             `buffer` and `coalesce` are two settings because pdl conflated
             them into one and got the bug at Discord.ts:96.

  error.rs
    Error
      ← new module, no old equivalent
      Action: write fresh
      Notes: neither source repo has an error type; both log and continue.
             Shape follows shep-log-rotate's error.rs. Every variant
             implements core::error::Error, never std::error::Error.

  stop.rs
    Stop
      ← new module
      Action: port from shep-log-rotate/src/stop.rs
      Notes: 131 lines, ctrl-c only, already correct. Not reinvented.
             Replaces dp2's process.on('SIGTERM'|'SIGINT') at client.ts:52;
             shep owns this process's kill ladder.

  shepherd.rs
    Live / flock / describe / act / host_usage / section / subscribe
      ← was dp2/src/services/pm2.ts
      Action: rewrite
      Notes: THE only module that builds a Request, matching shep-log-rotate's
             rule about tick.rs. 227 lines of callback-wrapping collapse to
             typed awaits. Live holds a ReconnectingClient and a socket path,
             so its Debug is hand-written and redacted.
             Verb mapping: start/stop/restart/reload/delete take SelectorSpec;
             dump becomes SaveRoll; reloadLogs becomes Reopen; flush stays
             Flush and is reachable ONLY from an explicit /shep flush, since
             it truncates. getProcessList's dedupe-by-name (pm2.ts:107) is
             kept but now means folding lambs, not instances.

  names.rs
    Names
      ← new module, no old equivalent
      Action: write fresh
      Notes: BusEvent::LogOut carries { id, line } and no name. PM2's bus
             carried the name on every frame, so neither source repo needed
             this. Caches id -> name off ListFlock, refreshes on a
             process.start or process.delete event, and falls back to
             "sheep <id>" rather than dropping a line.

  stream/mod.rs
    run
      ← was pdl/src/app.ts:28
      Action: rewrite
      Notes: subscribe(["log.out", "log.err"]) instead of pm2.launchBus, one
             select! over the stream and a flush timer instead of two
             callbacks plus setInterval. Handles BusEvent::Dropped and
             Lagged, which pdl had no equivalent of: both become a visible
             "N lines dropped" notice rather than a silent gap.

  stream/buffer.rs
    Buffer::push / Buffer::drain
      ← was pdl/src/Discord.ts:72 (pushToBuffer) and :87 (collectLogs)
      Action: port + redesign
      Notes: the idea is good and the implementation has three defects to fix.
             (1) coalesce window reads config rather than a hardcoded 1000ms
             (Discord.ts:96). (2) drain returns EVERY complete group, not one
             (Discord.ts:87 drains one per flush, so a backlog never clears).
             (3) a bounded buffer that drops oldest with a count, since the
             old one grows without limit. Self-filtering by process name
             (Discord.ts:73) is replaced by filtering on the dog's own id from
             the handshake, which a rename cannot break.

  stream/chunk.rs
    chunk / Embed building
      ← was pdl/src/Discord.ts:40 (createMessage)
      Action: port + redesign
      Notes: 4,095-char chunking and the "(i/n)" suffix carry over verbatim.
             Adds the cap pdl never had: Discord refuses more than 10 embeds
             per message, which is why Discord.ts:67 retries the same failed
             batch forever. Strips ANSI on the way in, as pdl did.

  bot/mod.rs
    Bot / State
      ← was dp2/src/discord/client.ts:9
      Action: port + redesign
      Notes: client.ctx is a discord.js module augmentation (dp2/src/types.ts:22)
             with no Rust equivalent; it becomes an explicit State the handler
             holds. Shutdown moves to stop.rs.

  bot/command.rs
    Command trait / registry / register
      ← was dp2/src/types.ts:12 and dp2/src/discord/register.ts
      Action: port
      Notes: data / run / optional autocomplete / optional button. The `modal`
             hook has no implementor in either repo and is not ported on spec.
             The NODE_ENV=development skip at register.ts:12 is dropped: it
             silently hides a newly added command.

  bot/interaction.rs
    dispatch
      ← was dp2/src/discord/events/interaction.ts:7
      Action: port
      Notes: the name-derivation ladder at interaction.ts:12 becomes a match on
             serenity's Interaction enum, which is where Rust wins outright.
             The swallowed follow-up failure at interaction.ts:55 is fixed: a
             failure to report a failure gets a stderr line.

  bot/embed.rs
    process_embed / process_buttons
      ← was dp2/src/discord/embeds/process.ts
      Action: port + redesign
      Notes: six of twelve fields have no ProcessInfo source and are dropped:
             version, namespace, exec_mode, max_memory_restart, autorestart,
             interpreter. Kept: status, uptime, cpu, memory, restarts, pid, id.
             Added from shep: instance, lambs, fold, smit, dog, and dog_stale
             when set. Colour-by-status carries over.
             Buttons change key: custom_id becomes "<verb>:<id>" using the
             numeric ProcessInfo.id rather than the name. Discord caps a
             custom_id at 100 bytes and shep does not constrain a sheep name,
             so the name scheme at process.ts:127 is not safe to reuse, and
             SelectorSpec::Id exists to receive it.

  bot/channel.rs
    resolve / clear / delete_one
      ← was dp2/src/discord/utils.ts
      Action: port + redesign
      Notes: getMonitorChannel returns null at four separate failures with only
             a log line (utils.ts:9); becomes Result. clearMonitorChannel's
             14-day bulkDelete split (utils.ts:49) is a real Discord constraint
             and carries over exactly.

  bot/monitor.rs
    update_one / update_all / Monitor
      ← was dp2/src/discord/monitor.ts
      Action: port
      Notes: the inFlight guard at monitor.ts:12 is the best idea in either
             repo and carries over as a per-name entry in a shared map. The
             setInterval handle becomes a JoinHandle plus a cancellation
             token.

  bot/commands/shep.rs
    /shep
      ← was dp2/src/discord/commands/pm2.ts
      Action: port + rename
      Notes: the most-churned file in either repo, 11 commits. Renamed /pm2 to
             /shep, and the verbs follow shep's own: reloadlogs becomes reopen,
             dump becomes save. `name` becomes required for the verbs that need
             one, instead of being caught as a thrown error at the service
             layer (pm2.ts:157). Autocomplete over the live flock is kept as is.

  bot/commands/monitor.rs
    /monitor
      ← was dp2/src/discord/commands/monitor.ts
      Action: port
      Notes: start / update / stop, the 0.25-minute floor, and the button
             router all carry over unchanged.

  bot/commands/system.rs
    /system
      ← was dp2/src/discord/commands/system.ts + dp2/src/services/system.ts
      Action: rewrite
      Notes: 148 lines become roughly 20. Request::HostUsage supplies cpu and
             memory; shep-core's MemSize and UpDuration render them. HostUsage
             also carries disk_bytes_per_second and network_bytes_per_second,
             which the old embed never had.
```

## Bulk 1:1 ports

| Old | New | Notes |
|-----|-----|-------|
| `pdl/src/Discord.ts:28` `getTitle` | `bot/embed.rs` | Same string, minus the `version` clause that has no source. |
| `pdl/src/Discord.ts:36` `ensureString` | dropped | `BusEvent::LogOut.line` is already a `String`. |
| `dp2/src/services/pm2.ts:20` `PAST_TENSE_MAP` | `shepherd.rs` | Same table, same replies. |
| `dp2/src/discord/events/ready.ts:8` `REFRESH_EVENTS` | `bot/monitor.rs` | A `ProcessEventKind` match instead of a `Set<string>`. |
| `shep-log-rotate/src/stop.rs` | `src/stop.rs` | Copied, not rewritten. |
| `shep-log-rotate/src/test_support.rs` | `src/test_support.rs` | `assert_no_dashes`, which this crate needs for the same reason. |

## Dropped

- `dp2/src/services/system.ts` (131 LOC) — `Request::HostUsage`. The module-global `lastSnapshot` race at `system.ts:27` goes with it.
- `dp2/src/services/logger.ts` (42 LOC) — a dog writes to stdout and shep captures it.
- `dp2/src/discord/commands/index.ts`, `dp2/src/discord/events/index.ts` — barrel re-exports.
- `dp2/config/default.json` — `[discord]` in `dogs.toml`.
- `pdl/src/types.ts` `Config` event booleans, and `app.ts:51`/`:59`/`:67` — the whole event-alerting path. `bark` owns it.
- `pdl/src/app.ts:14` `buffer_seconds` clamp — replaced by schema-level bounds, so an out-of-range value is refused rather than silently replaced.

## New, with no old equivalent

- `src/names.rs` — PM2's bus named the process on every frame; shep's log frames carry an id.
- `src/error.rs` — neither repo has an error type.
- Handling of `BusEvent::Dropped` and `Lagged` — the daemon's ring is 1,024 events ([`bus.rs:25`](https://github.com/shep-pm/shep/blob/main/crates/shep-daemon/src/bus.rs)) and a slow subscriber is told it fell behind. PM2's bus had no such signal, so neither repo could report a gap.
- Tests. Both repos have none; the target is shep-log-rotate's tier, above 92% of lines, plus a `tests/probe.rs` that spawns the binary to pin that `probe` runs before the argument parser.

## Open design calls

These need a decision before or during the spec. They are not mappings.

1. **Monitor state across a restart.** shep restarts a dog and the interval plus the cached message ids are in memory. Candidates: rediscover from the channel's own messages on boot (self-healing, no new storage, and `clear` already fetches exactly that set), persist through `kv.json` (4 KiB value cap, documented as not a blob store), or make the monitor a `dogs.toml` setting written with `Request::SetDogConfig` and re-read through the `config.dog.discord` subscription. The third is the most shep-native and the one most likely to need a brainstorm.
2. **Whether `/monitor start` keeps clearing the channel.** Defensible on a dedicated channel, destructive on a shared one. A config flag is the cheap answer.
3. **Channel sends or webhooks for the log stream.** The map assumes channel ids: the bot is already connected, so one credential instead of two, and no second secret to redact. Webhooks would give the stream its own rate-limit budget and a per-message username, at the cost of a second secret in the section. Taken as channel ids on KISS; worth revisiting if the stream starts starving command replies.
4. **`serenity` version, feature set and dependency weight** are not pinned here. `rin-dependency-choices` has not been discharged on it yet, and it should be before the first `Cargo.toml`.
