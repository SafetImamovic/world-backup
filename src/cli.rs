use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use chrono::NaiveTime;
use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::backup::{BackupConfig, RetentionPolicy, TieredRetentionPolicy};

#[derive(Debug, Parser)]
#[command(
    name = "world-backup",
    version,
    about = "Back up a Minecraft server world to a timestamped snapshot or archive."
)]
pub struct Cli {
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Backup(BackupArgs),
    Run(RunArgs),
}

#[derive(Debug, Args)]
pub struct BackupArgs {
    #[command(flatten)]
    pub backup: BackupOptions,
}

#[derive(Debug, Args)]
pub struct RunArgs {
    #[command(flatten)]
    pub backup: BackupOptions,

    #[arg(
        long,
        env = "WORLD_BACKUP_INTERVAL",
        default_value = "1h",
        conflicts_with = "cron",
        help = "How often to run backups, for example 30m, 2h, or 1d."
    )]
    pub interval: Option<String>,

    #[arg(
        long,
        env = "WORLD_BACKUP_CRON",
        conflicts_with = "interval",
        help = "Cron expression using 5, 6, or 7 fields. Five-field cron is accepted and interpreted in local time."
    )]
    pub cron: Option<String>,

    #[arg(
        long,
        help = "Perform one backup immediately before waiting for the schedule."
    )]
    pub run_immediately: bool,

    #[arg(
        long,
        conflicts_with = "cron",
        help = "Perform one backup immediately, then align interval backups to exact local time boundaries from midnight. For example, with 30m at 09:39 the next run is 10:00."
    )]
    pub run_immediately_aligned: bool,

    #[arg(
        long,
        env = "WORLD_BACKUP_ALWAYS_BACKUP",
        help = "Run scheduled backups even when the Minecraft server does not appear to be running. By default, `run` checks `world/session.lock` and skips offline backups."
    )]
    pub always_backup: bool,
}

#[derive(Debug, Args, Clone)]
pub struct BackupOptions {
    #[arg(
        long,
        env = "WORLD_BACKUP_SOURCE",
        default_value = "world",
        help = "Path to the Minecraft world directory."
    )]
    pub source: PathBuf,

    #[arg(
        long,
        env = "WORLD_BACKUP_TARGET_DIR",
        help = "Directory where timestamped backups are written. Defaults to a sibling '<world>-backups' directory."
    )]
    pub target_dir: Option<PathBuf>,

    #[arg(
        long,
        env = "WORLD_BACKUP_NAME",
        help = "Name prefix for created backups. Defaults to the source directory name."
    )]
    pub name: Option<String>,

    #[arg(
        long,
        env = "WORLD_BACKUP_COMPRESSION",
        value_enum,
        default_value_t = CompressionFormat::Zip,
        help = "Archive format to create. 'none' stores an uncompressed snapshot directory."
    )]
    pub compression: CompressionFormat,

    #[arg(
        long,
        env = "WORLD_BACKUP_COMPRESSION_LEVEL",
        help = "Compression level. Zip and tar-gz use 0-9, tar-zst uses 1-22."
    )]
    pub compression_level: Option<i32>,

    #[arg(
        long,
        env = "WORLD_BACKUP_KEEP_LAST",
        conflicts_with_all = ["keep_recent", "keep_daily_for_days", "keep_daily_at"],
        help = "Keep only the newest N backups that match the configured backup name."
    )]
    pub keep_last: Option<usize>,

    #[arg(
        long,
        env = "WORLD_BACKUP_KEEP_RECENT",
        conflicts_with = "keep_last",
        help = "Keep the newest N backups at full fidelity before daily retention rules apply."
    )]
    pub keep_recent: Option<usize>,

    #[arg(
        long,
        env = "WORLD_BACKUP_KEEP_DAILY_FOR_DAYS",
        conflicts_with = "keep_last",
        help = "For backups older than the recent window, keep daily checkpoint backups for the past N local calendar days."
    )]
    pub keep_daily_for_days: Option<usize>,

    #[arg(
        long = "keep-daily-at",
        requires = "keep_daily_for_days",
        action = clap::ArgAction::Append,
        help = "Local checkpoint time to preserve for older daily backups, for example 00:00 or 12:00. Defaults to 00:00 and 12:00."
    )]
    pub keep_daily_at: Vec<String>,

    #[arg(
        long = "exclude",
        action = clap::ArgAction::Append,
        help = "Glob pattern, relative to the world root, to skip during backup. May be passed more than once."
    )]
    pub exclude: Vec<String>,

    #[arg(
        long,
        env = "WORLD_BACKUP_DAY_DIRECTORIES",
        help = "Store backups in local YYYY-MM-DD subdirectories inside the target directory."
    )]
    pub day_directories: bool,

    #[arg(
        long,
        help = "Include the world's session.lock file. By default it is skipped to avoid copying the live lock file."
    )]
    pub include_session_lock: bool,

    #[arg(
        long,
        env = "WORLD_BACKUP_PRE_COMMAND",
        help = "Shell command to execute before the backup starts."
    )]
    pub pre_command: Option<String>,

    #[arg(
        long,
        env = "WORLD_BACKUP_POST_COMMAND",
        help = "Shell command to execute after the backup attempt finishes, if the pre-command succeeded."
    )]
    pub post_command: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CompressionFormat {
    None,
    Zip,
    TarGz,
    TarZst,
}

impl CompressionFormat {
    pub fn extension(self) -> Option<&'static str> {
        match self {
            Self::None => None,
            Self::Zip => Some("zip"),
            Self::TarGz => Some("tar.gz"),
            Self::TarZst => Some("tar.zst"),
        }
    }
}

impl BackupOptions {
    pub fn backup_config(&self) -> Result<BackupConfig> {
        let source = self.source.canonicalize().with_context(|| {
            format!(
                "source directory '{}' does not exist",
                self.source.display()
            )
        })?;
        if !source.is_dir() {
            bail!("source '{}' is not a directory", source.display());
        }

        let target_dir = match &self.target_dir {
            Some(path) => path.clone(),
            None => default_target_dir(&source)?,
        };

        let name = match &self.name {
            Some(name) => sanitize_name(name),
            None => sanitize_name(
                source
                    .file_name()
                    .and_then(|part| part.to_str())
                    .unwrap_or("world"),
            ),
        };
        if name.is_empty() {
            bail!("backup name resolves to an empty string; provide --name explicitly");
        }

        if let Some(level) = self.compression_level {
            validate_compression_level(self.compression, level)?;
        }
        let retention = build_retention_policy(
            self.keep_last,
            self.keep_recent,
            self.keep_daily_for_days,
            &self.keep_daily_at,
        )?;

        Ok(BackupConfig {
            source,
            target_dir,
            name,
            compression: self.compression,
            compression_level: self.compression_level,
            retention,
            exclude: self.exclude.clone(),
            day_directories: self.day_directories,
            include_session_lock: self.include_session_lock,
            pre_command: self.pre_command.clone(),
            post_command: self.post_command.clone(),
        })
    }
}

fn default_target_dir(source: &Path) -> Result<PathBuf> {
    let world_name = source
        .file_name()
        .and_then(|part| part.to_str())
        .context("could not infer a world directory name from the source path")?;
    let parent = source
        .parent()
        .context("could not determine the parent directory for the source path")?;
    Ok(parent.join(format!("{world_name}-backups")))
}

fn sanitize_name(raw: &str) -> String {
    raw.chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => ch,
        })
        .collect::<String>()
        .trim()
        .trim_matches('.')
        .to_string()
}

fn validate_compression_level(format: CompressionFormat, level: i32) -> Result<()> {
    match format {
        CompressionFormat::None => {
            bail!("--compression-level is not valid when --compression none is used")
        }
        CompressionFormat::Zip | CompressionFormat::TarGz => {
            if !(0..=9).contains(&level) {
                bail!("compression level for {:?} must be between 0 and 9", format);
            }
        }
        CompressionFormat::TarZst => {
            if !(1..=22).contains(&level) {
                bail!("compression level for tar-zst must be between 1 and 22");
            }
        }
    }

    Ok(())
}

fn build_retention_policy(
    keep_last: Option<usize>,
    keep_recent: Option<usize>,
    keep_daily_for_days: Option<usize>,
    keep_daily_at: &[String],
) -> Result<RetentionPolicy> {
    if let Some(limit) = keep_last {
        if limit == 0 {
            bail!("--keep-last must be at least 1");
        }
        return Ok(RetentionPolicy::KeepLast(limit));
    }

    if matches!(keep_recent, Some(0)) {
        bail!("--keep-recent must be at least 1");
    }
    if matches!(keep_daily_for_days, Some(0)) {
        bail!("--keep-daily-for-days must be at least 1");
    }
    if keep_recent.is_none() && keep_daily_for_days.is_none() {
        return Ok(RetentionPolicy::None);
    }

    let checkpoint_times = if keep_daily_for_days.is_some() {
        parse_checkpoint_times(keep_daily_at)?
    } else if !keep_daily_at.is_empty() {
        bail!("--keep-daily-at requires --keep-daily-for-days");
    } else {
        Vec::new()
    };

    Ok(RetentionPolicy::Tiered(TieredRetentionPolicy {
        keep_recent: keep_recent.unwrap_or(0),
        keep_daily_for_days: keep_daily_for_days.unwrap_or(0),
        daily_checkpoints: checkpoint_times,
    }))
}

fn parse_checkpoint_times(raw_times: &[String]) -> Result<Vec<NaiveTime>> {
    let input = if raw_times.is_empty() {
        vec!["00:00".to_string(), "12:00".to_string()]
    } else {
        raw_times.to_vec()
    };

    let mut parsed = Vec::new();
    for raw in input {
        let parsed_time = NaiveTime::parse_from_str(&raw, "%H:%M:%S")
            .or_else(|_| NaiveTime::parse_from_str(&raw, "%H:%M"))
            .with_context(|| {
                format!("invalid checkpoint time '{raw}', expected HH:MM or HH:MM:SS")
            })?;
        if !parsed.contains(&parsed_time) {
            parsed.push(parsed_time);
        }
    }
    parsed.sort();
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use chrono::NaiveTime;

    use super::{
        CompressionFormat, RetentionPolicy, build_retention_policy, default_target_dir,
        sanitize_name, validate_compression_level,
    };
    use std::path::Path;

    #[test]
    fn default_target_dir_uses_world_name() {
        let source = if cfg!(windows) {
            Path::new(r"C:\server\world")
        } else {
            Path::new("/srv/server/world")
        };
        let target = default_target_dir(source).unwrap();
        assert_eq!(
            target.file_name().and_then(|part| part.to_str()),
            Some("world-backups")
        );
    }

    #[test]
    fn sanitize_name_replaces_invalid_path_characters() {
        assert_eq!(sanitize_name(r#"atm10:world?*"#), "atm10_world__");
    }

    #[test]
    fn rejects_invalid_zip_level() {
        assert!(validate_compression_level(CompressionFormat::Zip, 12).is_err());
    }

    #[test]
    fn rejects_level_for_uncompressed_snapshot() {
        assert!(validate_compression_level(CompressionFormat::None, 1).is_err());
    }

    #[test]
    fn builds_tiered_retention_with_default_checkpoint_times() {
        let retention = build_retention_policy(None, Some(48), Some(14), &[]).unwrap();
        match retention {
            RetentionPolicy::Tiered(policy) => {
                assert_eq!(policy.keep_recent, 48);
                assert_eq!(policy.keep_daily_for_days, 14);
                assert_eq!(
                    policy.daily_checkpoints,
                    vec![
                        NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
                        NaiveTime::from_hms_opt(12, 0, 0).unwrap()
                    ]
                );
            }
            other => panic!("unexpected retention policy: {other:?}"),
        }
    }

    #[test]
    fn rejects_zero_recent_retention() {
        assert!(build_retention_policy(None, Some(0), Some(14), &[]).is_err());
    }
}
