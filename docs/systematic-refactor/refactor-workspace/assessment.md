# Good/bad assessment

Every file here crosses a language boundary, so the skill's `Keep` and `Refactor in place` verdicts never apply and a column of `Rewrite` would say nothing. The useful axis is **how much of the idea survives**:

- **Port** — the logic carries over, only the syntax changes.
- **Port + redesign** — the idea survives, the shape changes because shep's data is a different shape.
- **Rewrite** — only the intent survives; the implementation has nothing worth carrying.
- **Delete** — nothing survives, because shep or Rust already supplies it.

## `pm2-discord-logger`

### `src/app.ts` (88 LOC)
- Verdict: **Rewrite**, partly **Delete**
- Evidence: `[STALE]` (last commit 2024-03-31) `[UNTESTED]`
- The event half (`pm2:kill`, `process:exception`, `process:event`, plus the nine config booleans) is deleted outright: `bark` owns it. The log half becomes a `subscribe(["log.out", "log.err"])` loop. `io.initModule` has no analogue; config arrives over the socket.
- Tension with goals: the file is the clearest statement of what the old program did, so it is the reference for the new loop even though not a line of it survives.
- Confidence: High

### `src/Discord.ts` (106 LOC)
- Verdict: **Rewrite**
- Evidence: `[UNTESTED]`, and four defects the trace found: the retry-forever batch at `Discord.ts:68`, the hardcoded 1,000 ms window that ignores `buffer_seconds` at `:96`, one coalesced group per flush at `:87`, and self-filtering against a literal process name at `:73`.
- The *idea* is the valuable part and it is genuinely good: buffer raw lines, coalesce a time window, chunk at Discord's embed limit, flush on a timer. Carry the idea, write the code fresh, and fix all four on the way. Discord's 10-embeds-per-message cap needs handling that was never there.
- Confidence: High

### `src/types.ts` (98 LOC)
- Verdict: **Delete**
- `Config` becomes the `Section` type in `config.rs`, with a different field set. `Data` is `BusEvent`, already defined in shep-core. `Role` survives as a two-variant stream enum rather than three.
- Confidence: High

## `discord-pm2`

### `src/services/system.ts` (131 LOC)
- Verdict: **Delete**
- The whole file is `os.cpus()` delta arithmetic, byte formatting and duration formatting. `Request::HostUsage` returns `cpu_percent`, `memory_used_bytes`, `memory_total_bytes`, and shep-core's `MemSize` and `UpDuration` already render. The module-global `lastSnapshot` bug at `system.ts:27` goes with it.
- Confidence: High

### `src/services/logger.ts` (42 LOC)
- Verdict: **Delete**
- `loglevel` plus chalk icons plus a timestamp prefix. A dog writes to stdout and shep captures it; shep-log-rotate uses bare `println!`/`eprintln!` and nothing more. `logToDiscord` has no caller worth keeping.
- Confidence: High

### `src/services/pm2.ts` (227 LOC)
- Verdict: **Rewrite**
- Evidence: `[HOT]` (6 commits) `[UNTESTED]`
- Two thirds of this file is wrapping callbacks in promises and defaulting missing fields, both of which vanish against a typed async request. What survives as decisions rather than code: dedupe-by-name for clustered apps (`pm2.ts:107`), the module-hiding filter, and the past-tense reply strings. `Flush` needs a note: shep-log-rotate forbids `Request::Flush` outright with a source-scanning test, because it truncates logs. Here it is a verb an operator explicitly asked for, so it is allowed, but it must never be reachable except from an explicit `/shep flush`.
- Confidence: High

### `src/discord/embeds/process.ts` (133 LOC)
- Verdict: **Port + redesign**
- Six of twelve fields have no `ProcessInfo` source. The button row, the `CUSTOM_ID_DELIMITER` scheme and the colour-by-status rule all carry over unchanged. The note at `process.ts:19` that "PM2 process names cannot contain colons" needs re-checking against shep's own name grammar before the delimiter is reused.
- Confidence: Medium, because the replacement field set is a design call rather than a mapping.

### `src/discord/monitor.ts` (82 LOC)
- Verdict: **Port**
- Evidence: `[UNTESTED]`, but only 1 commit, which here reads as "written once and correct" rather than neglected.
- The `inFlight` guard at `monitor.ts:12` is the good idea in this repo: two concurrent updates for one process cannot both decide no message exists. It becomes a per-name entry in a `Mutex<HashMap<..>>` or equivalent. The rest is a straight port.
- Confidence: High

### `src/discord/utils.ts` (78 LOC)
- Verdict: **Port + redesign**
- `getMonitorChannel`'s null-at-every-failure shape becomes `Result`. `clearMonitorChannel`'s 14-day bulk-delete split is a real Discord API constraint and carries over verbatim. Whether it should run at all is an open question in `goals.md`.
- Confidence: Medium

### `src/discord/events/interaction.ts` (59 LOC)
- Verdict: **Port**
- The dispatch shape maps onto serenity's `InteractionCreate` cleanly. The name-derivation ladder at `interaction.ts:12` becomes a match on the interaction enum, which is strictly better. Fix the swallowed follow-up failure at `:55`.
- Confidence: High

### `src/discord/events/ready.ts` (37 LOC)
- Verdict: **Port + redesign**
- The activity line and the refresh-on-event set carry over. `pm2.launchBus` inside the ready handler does not: the bus subscription belongs to the dog's own connection, which exists before the gateway does, so the ordering inverts. `REFRESH_EVENTS` maps onto `ProcessEventKind` as a real enum instead of a string set.
- Confidence: High

### `src/discord/commands/pm2.ts` (94 LOC)
- Verdict: **Port + rename**
- Evidence: `[HOT]` (11 commits, the most-churned file in either repo) `[UNTESTED]`
- Becomes `/shep`. Autocomplete over live names is worth keeping exactly as is. `name` should be required for the verbs that need one rather than caught as a thrown error at the service layer (`pm2.ts:46`). `reloadlogs` becomes `reopen`, `dump` becomes `save`, matching shep's own verbs.
- Confidence: High

### `src/discord/commands/monitor.ts` (92 LOC)
- Verdict: **Port**
- Subcommands, the 0.25-minute floor, and the button router all carry. The interval handle becomes a `JoinHandle` plus a cancellation token rather than a `setInterval` id.
- Confidence: High

### `src/discord/commands/system.ts` (17 LOC)
- Verdict: **Port + simplify**
- Seventeen lines calling a function that is itself being deleted. Rebuild on `HostUsage`, which also carries `disk_bytes_per_second` and `network_bytes_per_second` that the old embed never had.
- Confidence: High

### `src/discord/client.ts` (56 LOC)
- Verdict: **Port + redesign**
- `client.ctx` is a discord.js module augmentation, which has no Rust equivalent; it becomes an explicit state struct passed in. The SIGTERM/SIGINT shutdown is replaced by shep's own kill ladder plus a ctrl-c handler like `stop::Stop`.
- Confidence: High

### `src/discord/register.ts` (38 LOC)
- Verdict: **Port**
- serenity has the same guild-commands set call. Drop the `NODE_ENV=development` skip at `register.ts:12`, which is a footgun that silently hides a new command.
- Confidence: High

### `src/types.ts` (33 LOC)
- Verdict: **Port + redesign**
- The `Command` contract is worth keeping as a trait: `data`, `run`, and optional `autocomplete` / `button`. The `modal` hook has no implementor in either repo and should not be ported on spec.
- Confidence: High

### `src/discord/commands/index.ts`, `src/discord/events/index.ts` (5 LOC together)
- Verdict: **Delete**
- Barrel re-exports. Rust modules do this without a file.
- Confidence: High

### `config/default.json`
- Verdict: **Delete**
- Replaced by `[discord]` in `dogs.toml`, served over the socket.
- Confidence: High

## Tally

11 Port or Port+redesign, 3 Rewrite, 7 Delete.

The deletions are the headline: roughly 400 of 1,438 LOC exists only because PM2 had no typed protocol and Node has no host metrics, and shep supplies both.
