use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Local, NaiveDateTime, NaiveTime, Utc};
use flate2::{Compression, write::GzEncoder};
use globset::{Glob, GlobSet, GlobSetBuilder};
use log::{debug, info};
use tar::Builder as TarBuilder;
use tempfile::Builder as TempDirBuilder;
use walkdir::{DirEntry, WalkDir};
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

use crate::cli::CompressionFormat;

#[derive(Debug, Clone)]
pub struct BackupConfig {
    pub source: PathBuf,
    pub target_dir: PathBuf,
    pub name: String,
    pub compression: CompressionFormat,
    pub compression_level: Option<i32>,
    pub retention: RetentionPolicy,
    pub exclude: Vec<String>,
    pub include_session_lock: bool,
    pub pre_command: Option<String>,
    pub post_command: Option<String>,
}

#[derive(Debug, Clone)]
pub enum RetentionPolicy {
    None,
    KeepLast(usize),
    Tiered(TieredRetentionPolicy),
}

#[derive(Debug, Clone)]
pub struct TieredRetentionPolicy {
    pub keep_recent: usize,
    pub keep_daily_for_days: usize,
    pub daily_checkpoints: Vec<NaiveTime>,
}

#[derive(Debug)]
pub struct BackupSummary {
    pub path: PathBuf,
    pub bytes: u64,
    pub deleted: Vec<PathBuf>,
}

#[derive(Debug)]
struct BackupArtifact {
    base_name: String,
    final_path: PathBuf,
}

#[derive(Debug, Clone)]
struct ManagedBackup {
    path: PathBuf,
    timestamp_utc: DateTime<Utc>,
    timestamp_local: DateTime<Local>,
}

#[derive(Debug)]
struct Exclusions {
    matcher: GlobSet,
}

impl Exclusions {
    fn new(config: &BackupConfig) -> Result<Self> {
        let mut builder = GlobSetBuilder::new();
        if !config.include_session_lock {
            builder.add(
                Glob::new("session.lock").context("failed to compile built-in exclude pattern")?,
            );
        }
        for pattern in &config.exclude {
            let normalized = pattern.replace('\\', "/");
            builder.add(
                Glob::new(&normalized)
                    .with_context(|| format!("invalid exclude pattern '{pattern}'"))?,
            );
        }

        Ok(Self {
            matcher: builder.build().context("failed to build exclude matcher")?,
        })
    }

    fn is_match(&self, relative_path: &Path) -> bool {
        if relative_path.as_os_str().is_empty() {
            return false;
        }
        self.matcher.is_match(normalize_path(relative_path))
    }
}

pub fn perform_backup(config: BackupConfig) -> Result<BackupSummary> {
    let target_dir = if config.target_dir.is_absolute() {
        config.target_dir.clone()
    } else {
        std::env::current_dir()
            .context("failed to determine the current working directory")?
            .join(&config.target_dir)
    };

    fs::create_dir_all(&target_dir).with_context(|| {
        format!(
            "failed to create target directory '{}'",
            target_dir.display()
        )
    })?;
    ensure_target_is_not_inside_source(&config.source, &target_dir)?;

    let pre_succeeded = if let Some(command) = &config.pre_command {
        run_shell_command(command, "pre-backup")?;
        true
    } else {
        false
    };

    let backup_result = perform_backup_inner(&config, &target_dir);
    let post_result = if pre_succeeded {
        if let Some(command) = &config.post_command {
            run_shell_command(command, "post-backup")
        } else {
            Ok(())
        }
    } else {
        Ok(())
    };

    match (backup_result, post_result) {
        (Ok(summary), Ok(())) => Ok(summary),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(backup_error), Err(post_error)) => Err(backup_error.context(format!(
            "backup failed and post-backup hook also failed: {post_error:#}"
        ))),
    }
}

fn perform_backup_inner(config: &BackupConfig, target_dir: &Path) -> Result<BackupSummary> {
    let exclusions = Exclusions::new(config)?;
    let artifact = build_artifact(target_dir, &config.name, config.compression);
    info!("starting backup of {}", config.source.display());
    info!("writing backup to {}", artifact.final_path.display());

    let staging = TempDirBuilder::new()
        .prefix(".world-backup-")
        .tempdir_in(target_dir)
        .with_context(|| {
            format!(
                "failed to create staging directory in '{}'",
                target_dir.display()
            )
        })?;
    let snapshot_root = staging.path().join(&artifact.base_name);
    fs::create_dir_all(&snapshot_root).with_context(|| {
        format!(
            "failed to create snapshot directory '{}'",
            snapshot_root.display()
        )
    })?;

    copy_source_to_snapshot(&config.source, &snapshot_root, &exclusions)?;

    match config.compression {
        CompressionFormat::None => {
            fs::rename(&snapshot_root, &artifact.final_path).with_context(|| {
                format!(
                    "failed to move snapshot '{}' to '{}'",
                    snapshot_root.display(),
                    artifact.final_path.display()
                )
            })?;
        }
        CompressionFormat::Zip => {
            write_zip_archive(
                &snapshot_root,
                &artifact.final_path,
                config.compression_level,
            )?;
        }
        CompressionFormat::TarGz => {
            write_tar_gz_archive(
                &snapshot_root,
                &artifact.final_path,
                config.compression_level,
            )?;
        }
        CompressionFormat::TarZst => {
            write_tar_zst_archive(
                &snapshot_root,
                &artifact.final_path,
                config.compression_level,
            )?;
        }
    }

    let bytes = artifact_size(&artifact.final_path)?;
    let deleted = enforce_retention(
        target_dir,
        &config.name,
        &config.retention,
        &artifact.final_path,
    )?;

    Ok(BackupSummary {
        path: artifact.final_path,
        bytes,
        deleted,
    })
}

fn build_artifact(target_dir: &Path, name: &str, compression: CompressionFormat) -> BackupArtifact {
    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let base_name = format!("{name}-{timestamp}");
    let final_name = match compression.extension() {
        Some(extension) => format!("{base_name}.{extension}"),
        None => base_name.clone(),
    };

    BackupArtifact {
        base_name,
        final_path: target_dir.join(final_name),
    }
}

fn ensure_target_is_not_inside_source(source: &Path, target_dir: &Path) -> Result<()> {
    let canonical_target = target_dir
        .canonicalize()
        .unwrap_or_else(|_| target_dir.to_path_buf());
    if canonical_target.starts_with(source) {
        bail!(
            "target directory '{}' cannot be inside the source world '{}'",
            canonical_target.display(),
            source.display()
        );
    }
    Ok(())
}

fn copy_source_to_snapshot(
    source: &Path,
    snapshot_root: &Path,
    exclusions: &Exclusions,
) -> Result<()> {
    let walker = WalkDir::new(source)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| should_visit(entry, source, exclusions));

    for entry in walker {
        let entry = entry
            .with_context(|| format!("failed to read entries under '{}'", source.display()))?;
        let relative = entry.path().strip_prefix(source).with_context(|| {
            format!(
                "failed to calculate a relative path for '{}'",
                entry.path().display()
            )
        })?;
        if relative.as_os_str().is_empty() {
            continue;
        }

        let destination = snapshot_root.join(relative);
        let file_type = entry.file_type();
        if file_type.is_symlink() {
            bail!(
                "symlinks are not supported in backups: '{}'",
                entry.path().display()
            );
        }

        if file_type.is_dir() {
            fs::create_dir_all(&destination)
                .with_context(|| format!("failed to create '{}'", destination.display()))?;
            continue;
        }

        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create '{}'", parent.display()))?;
        }

        fs::copy(entry.path(), &destination).with_context(|| {
            format!(
                "failed to copy '{}' to '{}'",
                entry.path().display(),
                destination.display()
            )
        })?;
        let permissions = entry
            .metadata()
            .with_context(|| format!("failed to read metadata for '{}'", entry.path().display()))?
            .permissions();
        fs::set_permissions(&destination, permissions)
            .with_context(|| format!("failed to set permissions on '{}'", destination.display()))?;
    }

    Ok(())
}

fn should_visit(entry: &DirEntry, source: &Path, exclusions: &Exclusions) -> bool {
    match entry.path().strip_prefix(source) {
        Ok(relative) => !exclusions.is_match(relative),
        Err(_) => true,
    }
}

fn write_zip_archive(snapshot_root: &Path, final_path: &Path, level: Option<i32>) -> Result<()> {
    let partial_path = partial_path(final_path);
    let file = File::create(&partial_path)
        .with_context(|| format!("failed to create '{}'", partial_path.display()))?;
    let writer = BufWriter::new(file);
    let mut zip = zip::ZipWriter::new(writer);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(level.map(i64::from));

    let archive_root_parent = snapshot_root
        .parent()
        .context("snapshot root does not have a parent directory")?;
    for entry in WalkDir::new(snapshot_root) {
        let entry =
            entry.with_context(|| format!("failed to traverse '{}'", snapshot_root.display()))?;
        let relative = entry
            .path()
            .strip_prefix(archive_root_parent)
            .with_context(|| format!("failed to normalize '{}'", entry.path().display()))?;
        let name = normalize_path(relative);

        if entry.file_type().is_dir() {
            zip.add_directory(format!("{name}/"), options)
                .with_context(|| format!("failed to add directory '{name}' to zip archive"))?;
            continue;
        }

        zip.start_file(name.clone(), options)
            .with_context(|| format!("failed to add '{name}' to zip archive"))?;
        let mut file = File::open(entry.path())
            .with_context(|| format!("failed to open '{}'", entry.path().display()))?;
        std::io::copy(&mut file, &mut zip)
            .with_context(|| format!("failed to write '{name}' into zip archive"))?;
    }

    let writer = zip.finish().context("failed to finalize zip archive")?;
    let mut file = writer.into_inner().context("failed to flush zip archive")?;
    file.flush()
        .context("failed to flush zip archive to disk")?;
    fs::rename(&partial_path, final_path).with_context(|| {
        format!(
            "failed to rename '{}' to '{}'",
            partial_path.display(),
            final_path.display()
        )
    })?;
    Ok(())
}

fn write_tar_gz_archive(snapshot_root: &Path, final_path: &Path, level: Option<i32>) -> Result<()> {
    let partial_path = partial_path(final_path);
    let file = File::create(&partial_path)
        .with_context(|| format!("failed to create '{}'", partial_path.display()))?;
    let writer = BufWriter::new(file);
    let encoder = GzEncoder::new(writer, gzip_level(level));
    let mut tar = TarBuilder::new(encoder);
    let base_name = snapshot_root
        .file_name()
        .and_then(|part| part.to_str())
        .context("snapshot root is missing a valid directory name")?;
    tar.append_dir_all(base_name, snapshot_root)
        .with_context(|| format!("failed to archive '{}'", snapshot_root.display()))?;
    let encoder = tar
        .into_inner()
        .context("failed to finalize tar.gz archive")?;
    let mut writer = encoder.finish().context("failed to finish gzip stream")?;
    writer
        .flush()
        .context("failed to flush tar.gz archive to disk")?;
    fs::rename(&partial_path, final_path).with_context(|| {
        format!(
            "failed to rename '{}' to '{}'",
            partial_path.display(),
            final_path.display()
        )
    })?;
    Ok(())
}

fn write_tar_zst_archive(
    snapshot_root: &Path,
    final_path: &Path,
    level: Option<i32>,
) -> Result<()> {
    let partial_path = partial_path(final_path);
    let file = File::create(&partial_path)
        .with_context(|| format!("failed to create '{}'", partial_path.display()))?;
    let writer = BufWriter::new(file);
    let encoder = zstd::stream::write::Encoder::new(writer, zstd_level(level))
        .context("failed to initialize zstd encoder")?;
    let mut tar = TarBuilder::new(encoder);
    let base_name = snapshot_root
        .file_name()
        .and_then(|part| part.to_str())
        .context("snapshot root is missing a valid directory name")?;
    tar.append_dir_all(base_name, snapshot_root)
        .with_context(|| format!("failed to archive '{}'", snapshot_root.display()))?;
    let encoder = tar
        .into_inner()
        .context("failed to finalize tar.zst archive")?;
    let writer = encoder.finish().context("failed to finish zstd stream")?;
    let mut file = writer
        .into_inner()
        .context("failed to flush tar.zst archive from buffered writer")?;
    file.flush()
        .context("failed to flush tar.zst archive to disk")?;
    fs::rename(&partial_path, final_path).with_context(|| {
        format!(
            "failed to rename '{}' to '{}'",
            partial_path.display(),
            final_path.display()
        )
    })?;
    Ok(())
}

fn partial_path(final_path: &Path) -> PathBuf {
    let file_name = final_path
        .file_name()
        .and_then(|part| part.to_str())
        .unwrap_or("backup");
    let partial = format!(".{file_name}.partial");
    final_path.with_file_name(partial)
}

fn gzip_level(level: Option<i32>) -> Compression {
    match level {
        Some(value) => Compression::new(value.clamp(0, 9) as u32),
        None => Compression::default(),
    }
}

fn zstd_level(level: Option<i32>) -> i32 {
    level.unwrap_or(3).clamp(1, 22)
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn artifact_size(path: &Path) -> Result<u64> {
    if path.is_file() {
        return Ok(path
            .metadata()
            .with_context(|| format!("failed to read metadata for '{}'", path.display()))?
            .len());
    }

    let mut total = 0_u64;
    for entry in WalkDir::new(path) {
        let entry = entry.with_context(|| format!("failed to traverse '{}'", path.display()))?;
        if entry.file_type().is_file() {
            total += entry
                .metadata()
                .with_context(|| {
                    format!("failed to read metadata for '{}'", entry.path().display())
                })?
                .len();
        }
    }
    Ok(total)
}

fn enforce_retention(
    target_dir: &Path,
    name: &str,
    retention: &RetentionPolicy,
    newest_path: &Path,
) -> Result<Vec<PathBuf>> {
    if matches!(retention, RetentionPolicy::None) {
        return Ok(Vec::new());
    }

    let backups = collect_managed_backups(target_dir, name)?;
    let keep = select_backups_to_keep(retention, &backups, newest_path);

    let mut deleted = Vec::new();
    for backup in backups {
        if keep.contains(&backup.path) {
            continue;
        }
        debug!("deleting old backup '{}'", backup.path.display());
        if backup.path.is_dir() {
            fs::remove_dir_all(&backup.path).with_context(|| {
                format!("failed to delete old backup '{}'", backup.path.display())
            })?;
        } else {
            fs::remove_file(&backup.path).with_context(|| {
                format!("failed to delete old backup '{}'", backup.path.display())
            })?;
        }
        deleted.push(backup.path);
    }

    Ok(deleted)
}

fn collect_managed_backups(target_dir: &Path, name: &str) -> Result<Vec<ManagedBackup>> {
    let prefix = format!("{name}-");
    let mut backups = Vec::new();
    for entry in fs::read_dir(target_dir)
        .with_context(|| format!("failed to read '{}'", target_dir.display()))?
    {
        let entry = entry
            .with_context(|| format!("failed to read entries in '{}'", target_dir.display()))?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|part| part.to_str()) else {
            continue;
        };
        let Some(timestamp_utc) = parse_backup_timestamp(&prefix, file_name) else {
            continue;
        };

        backups.push(ManagedBackup {
            path,
            timestamp_local: timestamp_utc.with_timezone(&Local),
            timestamp_utc,
        });
    }

    backups.sort_by(|left, right| {
        right
            .timestamp_utc
            .cmp(&left.timestamp_utc)
            .then_with(|| right.path.cmp(&left.path))
    });
    Ok(backups)
}

fn parse_backup_timestamp(prefix: &str, file_name: &str) -> Option<DateTime<Utc>> {
    let suffix = file_name.strip_prefix(prefix)?;
    if suffix.len() < 16 {
        return None;
    }

    let timestamp = &suffix[..16];
    let remainder = &suffix[16..];
    if !remainder.is_empty() && !remainder.starts_with('.') {
        return None;
    }

    NaiveDateTime::parse_from_str(timestamp, "%Y%m%dT%H%M%SZ")
        .ok()
        .map(|timestamp| DateTime::from_naive_utc_and_offset(timestamp, Utc))
}

fn select_backups_to_keep(
    retention: &RetentionPolicy,
    backups: &[ManagedBackup],
    newest_path: &Path,
) -> HashSet<PathBuf> {
    let mut keep = HashSet::new();
    keep.insert(newest_path.to_path_buf());

    match retention {
        RetentionPolicy::None => keep,
        RetentionPolicy::KeepLast(limit) => {
            for backup in backups.iter().take(*limit) {
                keep.insert(backup.path.clone());
            }
            keep
        }
        RetentionPolicy::Tiered(policy) => {
            for backup in backups.iter().take(policy.keep_recent) {
                keep.insert(backup.path.clone());
            }

            if policy.keep_daily_for_days == 0 || policy.daily_checkpoints.is_empty() {
                return keep;
            }

            let reference_day = backups
                .first()
                .map(|backup| backup.timestamp_local.date_naive())
                .unwrap_or_else(|| Local::now().date_naive());
            let mut by_day: HashMap<_, Vec<&ManagedBackup>> = HashMap::new();
            for backup in backups.iter().skip(policy.keep_recent) {
                let backup_day = backup.timestamp_local.date_naive();
                let day_age = reference_day.signed_duration_since(backup_day).num_days();
                if (1..=policy.keep_daily_for_days as i64).contains(&day_age) {
                    by_day.entry(backup_day).or_default().push(backup);
                }
            }

            for daily_backups in by_day.values() {
                for checkpoint in &policy.daily_checkpoints {
                    if let Some(selected) = select_backup_for_checkpoint(daily_backups, *checkpoint)
                    {
                        keep.insert(selected.path.clone());
                    }
                }
            }

            keep
        }
    }
}

fn select_backup_for_checkpoint<'a>(
    backups: &[&'a ManagedBackup],
    checkpoint: NaiveTime,
) -> Option<&'a ManagedBackup> {
    backups.iter().copied().min_by(|left, right| {
        let left_delta = left
            .timestamp_local
            .time()
            .signed_duration_since(checkpoint)
            .num_seconds()
            .abs();
        let right_delta = right
            .timestamp_local
            .time()
            .signed_duration_since(checkpoint)
            .num_seconds()
            .abs();
        left_delta
            .cmp(&right_delta)
            .then_with(|| right.timestamp_utc.cmp(&left.timestamp_utc))
    })
}

fn run_shell_command(command: &str, label: &str) -> Result<()> {
    info!("running {label} hook");
    #[cfg(windows)]
    let status = Command::new("cmd")
        .args(["/C", command])
        .status()
        .with_context(|| format!("failed to run {label} hook"))?;

    #[cfg(not(windows))]
    let status = Command::new("sh")
        .args(["-c", command])
        .status()
        .with_context(|| format!("failed to run {label} hook"))?;

    if !status.success() {
        bail!("{label} hook exited with status {status}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use chrono::{Duration, Local, NaiveTime, TimeZone, Utc};
    use tempfile::tempdir;

    use super::{
        BackupConfig, ManagedBackup, RetentionPolicy, TieredRetentionPolicy, perform_backup,
        select_backups_to_keep,
    };
    use crate::cli::CompressionFormat;

    #[test]
    fn creates_uncompressed_snapshot_and_skips_session_lock() {
        let source_root = tempdir().unwrap();
        let source = source_root.path().join("world");
        fs::create_dir_all(source.join("region")).unwrap();
        fs::write(source.join("level.dat"), b"level").unwrap();
        fs::write(source.join("session.lock"), b"lock").unwrap();
        fs::write(source.join("region").join("r.0.0.mca"), b"chunk").unwrap();

        let target_root = tempdir().unwrap();
        let config = BackupConfig {
            source: source.clone(),
            target_dir: target_root.path().to_path_buf(),
            name: "atm10".to_string(),
            compression: CompressionFormat::None,
            compression_level: None,
            retention: RetentionPolicy::None,
            exclude: Vec::new(),
            include_session_lock: false,
            pre_command: None,
            post_command: None,
        };

        let summary = perform_backup(config).unwrap();
        assert!(summary.path.is_dir());
        assert!(summary.path.join("level.dat").exists());
        assert!(summary.path.join("region").join("r.0.0.mca").exists());
        assert!(!summary.path.join("session.lock").exists());
    }

    #[test]
    fn retention_removes_older_matching_backups() {
        let source_root = tempdir().unwrap();
        let source = source_root.path().join("world");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("level.dat"), b"level").unwrap();

        let target_root = tempdir().unwrap();
        let old_one = target_root.path().join("atm10-20200101T000000Z.zip");
        let old_two = target_root.path().join("atm10-20200102T000000Z.zip");
        let keep_other = target_root.path().join("other-20200101T000000Z.zip");
        fs::write(&old_one, b"a").unwrap();
        fs::write(&old_two, b"b").unwrap();
        fs::write(&keep_other, b"c").unwrap();

        let config = BackupConfig {
            source,
            target_dir: target_root.path().to_path_buf(),
            name: "atm10".to_string(),
            compression: CompressionFormat::Zip,
            compression_level: Some(1),
            retention: RetentionPolicy::KeepLast(2),
            exclude: Vec::new(),
            include_session_lock: false,
            pre_command: None,
            post_command: None,
        };

        let summary = perform_backup(config).unwrap();
        assert!(summary.path.exists());
        assert_eq!(summary.deleted.len(), 1);
        assert!(!old_one.exists());
        assert!(old_two.exists());
        assert!(keep_other.exists());
    }

    #[test]
    fn tiered_retention_keeps_recent_window_and_daily_checkpoints() {
        let start_local = Local
            .with_ymd_and_hms(2026, 2, 10, 0, 0, 0)
            .single()
            .unwrap();
        let mut backups = Vec::new();
        for step in 0..96 {
            let local_time = start_local + Duration::minutes((step * 30) as i64);
            backups.push(managed_backup(local_time.with_timezone(&Utc)));
        }
        backups.sort_by(|left, right| right.timestamp_utc.cmp(&left.timestamp_utc));

        let newest_path = backups.first().unwrap().path.clone();
        let keep = select_backups_to_keep(
            &RetentionPolicy::Tiered(TieredRetentionPolicy {
                keep_recent: 48,
                keep_daily_for_days: 14,
                daily_checkpoints: vec![
                    NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
                    NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
                ],
            }),
            &backups,
            &newest_path,
        );

        assert_eq!(keep.len(), 50);

        let previous_midnight = managed_backup(
            Local
                .with_ymd_and_hms(2026, 2, 10, 0, 0, 0)
                .single()
                .unwrap()
                .with_timezone(&Utc),
        );
        let previous_noon = managed_backup(
            Local
                .with_ymd_and_hms(2026, 2, 10, 12, 0, 0)
                .single()
                .unwrap()
                .with_timezone(&Utc),
        );
        let previous_extra = managed_backup(
            Local
                .with_ymd_and_hms(2026, 2, 10, 12, 30, 0)
                .single()
                .unwrap()
                .with_timezone(&Utc),
        );

        assert!(keep.contains(&previous_midnight.path));
        assert!(keep.contains(&previous_noon.path));
        assert!(!keep.contains(&previous_extra.path));
    }

    fn managed_backup(timestamp_utc: chrono::DateTime<Utc>) -> ManagedBackup {
        let file_name = format!("atm10-{}.zip", timestamp_utc.format("%Y%m%dT%H%M%SZ"));
        ManagedBackup {
            path: PathBuf::from(file_name),
            timestamp_local: timestamp_utc.with_timezone(&Local),
            timestamp_utc,
        }
    }
}
