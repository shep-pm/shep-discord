# Changelog

All notable changes to this crate are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] - 2026-09-21

### Changed

- Tell a section that is wrong from one nobody filled in

### Fixed

- Exit on a section this dog will not accept **(BREAKING)**
- Exit 4 on a refused dogs.toml, which is shep's own invalid_config **(BREAKING)**
- Stop a TOML parse error printing the bot token
- Print the line number of a bad section and nothing else
- Stop a pasted token reaching the log through a duration field
- Close the gaps in the message an operator reads on a fatal config
- Check written values before reporting a key nobody typed


## [0.2.0] - 2026-09-21

### Fixed

- Exit 6 when the shepherd refuses this dog's handshake **(BREAKING)**


## [0.1.1] - 2026-09-19

### Fixed

- Box the two serenity errors that escaped unboxed

