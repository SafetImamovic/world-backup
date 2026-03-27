# world-backup

`world-backup` is a Rust CLI that creates timestamped backups of a Minecraft server world. It can run once or stay running and back the world up on an interval or cron schedule.

## Features

- Creates a staged snapshot before writing the final backup artifact.
- Supports `zip`, `tar-gz`, `tar-zst`, or an uncompressed snapshot directory.
- Defaults to writing backups into a sibling `world-backups` directory.
- Accepts either `--interval 30m` style scheduling or `--cron "0 */6 * * *"` style scheduling.
- Skips `session.lock` by default.
- Can run pre/post shell hooks to integrate with server save commands or maintenance scripts.
- Can delete older matching backups with `--keep-last`.
- Can also keep a rolling recent window plus daily checkpoint backups with `--keep-recent`, `--keep-daily-for-days`, and `--keep-daily-at`.

## Build

```powershell
cargo build --release
```

The final binary will be at `target\release\world-backup.exe` on Windows.

## Examples

Back up the default `.\world` directory once into `.\world-backups` using zip compression:

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
  --keep-recent 48 `
  --keep-daily-for-days 14 `
  --keep-daily-at 00:00 `
  --keep-daily-at 12:00 `
  --run-immediately
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

Exclude additional content:

```powershell
world-backup backup --exclude "logs/**" --exclude "cache/**"
```

## Notes

- If you back up a live server, use the hook commands to flush or pause writes before the snapshot when possible.
- The target directory must not be inside the world directory.
- Backup names default to the source directory name. Use `--name` if you want a more descriptive prefix.
