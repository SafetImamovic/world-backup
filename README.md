# world-backup

## ATM10 Server Backups - How to Set Up?

If you are here because you are hosting an All the Mods 10 server and noticed there is no `simplebackups` mod in the server files: you are not missing anything. ATM10 server packs no longer reliably ship with SimpleBackups, and a lot of older advice is out of date.

I needed a straightforward backup setup for my own ATM10 server, so I went ahead and made this. `world-backup` is an external Rust CLI that creates timestamped backups of a Minecraft server world. It can run once or stay running and back the world up on an interval or cron schedule.

I made this as a standalone Rust program, not a Minecraft mod, because I wanted it independent of modpack health and lighter on the server itself. It runs as a compiled native binary instead of inside the JVM, and Rust is fast.

## Recommended ATM10 Setup

For most ATM10 servers, back up the `world` directory, not the entire server folder. That already includes the important live data like `level.dat`, `playerdata`, `region`, `DIM-1`, `DIM1`, `dimensions`, `poi`, `entities`, and mod-specific world data inside `world/`.

Use an external tool like this when you want:

- something pack-independent that still works even if the modpack does not include a backup mod
- the same workflow on Windows and Linux
- explicit retention rules and readable backup files
- backups that are not tied to a panel like AMP or a specific mod/plugin

If you already have a panel with reliable backups or you prefer a server-side backup mod, that can work too. This project is for the case where you just want a predictable answer that does not depend on ATM10 shipping the right mod.

## Features

- Creates a staged snapshot before writing the final backup artifact.
- Supports `zip`, `tar-gz`, `tar-zst`, or an uncompressed snapshot directory.
- Defaults to writing backups into a sibling `world-backups` directory.
- Uses human-readable local timestamp names such as `atm10-2026-03-27_10-15-00+0100.zip`.
- Can optionally place backups into per-day `YYYY-MM-DD` subdirectories.
- Accepts either `--interval 30m` style scheduling or `--cron "0 */6 * * *"` style scheduling.
- Can align interval schedules to exact wall-clock boundaries from local midnight after an immediate startup run.
- By default, scheduled `run` checks `world/session.lock` and skips backups when the server appears offline.
- Skips `session.lock` by default.
- Can run pre/post shell hooks to integrate with server save commands or maintenance scripts.
- Can delete older matching backups with `--keep-last`.
- Can also keep a rolling recent window plus daily checkpoint backups with `--keep-recent`, `--keep-daily-for-days`, and `--keep-daily-at`.

## Build

```powershell
cargo build --release
```

The final binary will be at `target\release\world-backup.exe` on Windows.

On Linux and macOS, the final binary will be at `target/release/world-backup`.

## Install

Install the binary into Cargo's global bin directory so you can run `world-backup` directly from your shell:

```powershell
cargo install --path .
```

On most systems this installs into Cargo's bin directory, typically `%USERPROFILE%\.cargo\bin` on Windows or `$HOME/.cargo/bin` on Linux and macOS. Make sure that directory is on your `PATH`.

## Examples

### PowerShell

```powershell
world-backup backup
```

Back up an ATM10 world every 30 minutes into a custom directory and keep the newest 24 backups:

```powershell
world-backup run `
  --source "C:\Users\User\Desktop\server-2.0.0\world" `
  --target-dir "D:\minecraft-backups\atm10" `
  --interval 30m `
  --compression zip `
  --keep-last 24 `
  --run-immediately
```

Keep the newest 48 half-hourly backups, then collapse older backups into midnight and noon checkpoints for the previous 14 days:

```powershell
world-backup run `
  --source "C:\Users\User\Desktop\server-2.0.0\world" `
  --target-dir "D:\minecraft-backups\atm10" `
  --interval 30m `
  --compression zip `
  --day-directories `
  --keep-recent 48 `
  --keep-daily-for-days 14 `
  --keep-daily-at 00:00 `
  --keep-daily-at 12:00 `
  --run-immediately
```

Run one backup immediately, then snap the next interval backup to the next exact boundary from local midnight. For example, with `15m` at `09:39:54`, the next run is `09:45`; with `30m`, the next run is `10:00`:

```powershell
world-backup run `
  --source "C:\Users\User\Desktop\server-2.0.0\world" `
  --interval 15m `
  --run-immediately-aligned
```

If you want scheduled backups even while the server is stopped, override the default skip behavior:

```powershell
world-backup run `
  --source "C:\Users\User\Desktop\server-2.0.0\world" `
  --interval 30m `
  --always-backup
```

Use a cron schedule instead of a fixed interval. Five-field cron is accepted and interpreted in local time:

```powershell
world-backup run `
  --source "C:\Users\User\Desktop\server-2.0.0\world" `
  --cron "0 */6 * * *" `
  --compression tar-zst
```

Run server-specific commands before and after the backup:

```powershell
world-backup backup `
  --source "C:\Users\User\Desktop\server-2.0.0\world" `
  --pre-command "echo save-all" `
  --post-command "echo backup completed"
```

Kitchen sink example with most of the available knobs:

```powershell
world-backup run `
  --source "C:\Users\User\Desktop\server-2.0.0\world" `
  --target-dir "D:\minecraft-backups\atm10" `
  --name "atm10-tts" `
  --compression tar-zst `
  --compression-level 10 `
  --interval 30m `
  --day-directories `
  --keep-recent 48 `
  --keep-daily-for-days 14 `
  --keep-daily-at 00:00 `
  --keep-daily-at 12:00 `
  --exclude "logs/**" `
  --exclude "cache/**" `
  --pre-command "echo save-all" `
  --post-command "echo save resume" `
  --run-immediately `
  -v
```

### Bash

Back up the default `./world` directory once into `./world-backups` using zip compression:

```bash
./target/release/world-backup backup
```

Back up an ATM10 world every 30 minutes into a custom directory and keep the newest 24 backups:

```bash
./target/release/world-backup run \
  --source "/srv/minecraft/atm10/world" \
  --target-dir "/srv/backups/atm10" \
  --interval 30m \
  --compression zip \
  --keep-last 24 \
  --run-immediately
```

Keep the newest 48 half-hourly backups, then collapse older backups into midnight and noon checkpoints for the previous 14 days:

```bash
./target/release/world-backup run \
  --source "/srv/minecraft/atm10/world" \
  --target-dir "/srv/backups/atm10" \
  --interval 30m \
  --compression zip \
  --day-directories \
  --keep-recent 48 \
  --keep-daily-for-days 14 \
  --keep-daily-at 00:00 \
  --keep-daily-at 12:00 \
  --run-immediately
```

Run one backup immediately, then snap the next interval backup to the next exact boundary from local midnight:

```bash
./target/release/world-backup run \
  --source "/srv/minecraft/atm10/world" \
  --interval 15m \
  --run-immediately-aligned
```

If you want scheduled backups even while the server is stopped, override the default skip behavior:

```bash
./target/release/world-backup run \
  --source "/srv/minecraft/atm10/world" \
  --interval 30m \
  --always-backup
```

Use a cron schedule instead of a fixed interval. Five-field cron is accepted and interpreted in local time:

```bash
./target/release/world-backup run \
  --source "/srv/minecraft/atm10/world" \
  --cron "0 */6 * * *" \
  --compression tar-zst
```

Kitchen sink example with most of the available knobs:

```bash
./target/release/world-backup run \
  --source "/srv/minecraft/atm10/world" \
  --target-dir "/srv/backups/atm10" \
  --name "atm10-tts" \
  --compression tar-zst \
  --compression-level 10 \
  --cron "0 */6 * * *" \
  --day-directories \
  --keep-recent 48 \
  --keep-daily-for-days 14 \
  --keep-daily-at 00:00 \
  --keep-daily-at 12:00 \
  --exclude "logs/**" \
  --exclude "cache/**" \
  --pre-command "rcon-cli save-all" \
  --post-command "echo backup finished" \
  --run-immediately \
  -v
```

## Notes

- For ATM10 specifically, backing up `world` is usually the right answer, not the whole server root.
- This is meant as the boring, reliable external option when the pack does not ship a backup mod and the wiki or old Reddit answers are outdated.
- Scheduled `run` uses the Minecraft `session.lock` file as the liveness check. If you want backups regardless of server state, use `--always-backup`.
- If you back up a live server, use the hook commands to flush or pause writes before the snapshot when possible.
- `--day-directories` stores backups under local date folders such as `2026-03-27\atm10-2026-03-27_10-15-00+0100.zip`.
- The target directory must not be inside the world directory.
- Backup names default to the source directory name. Use `--name` if you want a more descriptive prefix.
