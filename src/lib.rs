mod backup;
mod cli;
mod hooks;
mod schedule;
mod server_state;

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Local;
use clap::Parser;
use log::{error, info, warn};

use crate::backup::perform_backup;
use crate::cli::{Cli, Command};
use crate::hooks::init_logging;
use crate::schedule::ScheduleSpec;
use crate::server_state::world_appears_running;

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose)?;

    match cli.command {
        Command::Backup(args) => {
            let summary = perform_backup(args.backup.backup_config()?)?;
            info!("backup created at {}", summary.path.display());
            info!("backup size: {} bytes", summary.bytes);
            if !summary.deleted.is_empty() {
                info!("deleted {} older backup(s)", summary.deleted.len());
            }
        }
        Command::Run(args) => {
            let config = args.backup.backup_config()?;
            let schedule = ScheduleSpec::from_args(
                args.interval.as_deref(),
                args.cron.as_deref(),
                args.run_immediately_aligned,
            )?;
            if let Some(expression) = schedule.expression() {
                info!("using cron schedule {expression}");
            }

            let shutdown = install_ctrlc_handler()?;
            if args.run_immediately || args.run_immediately_aligned {
                match perform_scheduled_backup(&config, args.always_backup) {
                    Ok(ScheduledBackup::Performed(summary)) => {
                        info!("startup backup created at {}", summary.path.display())
                    }
                    Ok(ScheduledBackup::Skipped) => {
                        info!(
                            "startup backup skipped because the Minecraft server does not appear to be running"
                        )
                    }
                    Err(error) => error!("startup backup failed: {error:#}"),
                }
            }

            loop {
                if shutdown.load(Ordering::SeqCst) {
                    info!("shutdown requested; exiting scheduler");
                    break;
                }

                let next_run = schedule
                    .next_after(Local::now())
                    .context("failed to calculate the next scheduled run")?;
                info!("next backup scheduled for {}", next_run.to_rfc3339());

                wait_until(next_run, &shutdown);
                if shutdown.load(Ordering::SeqCst) {
                    info!("shutdown requested before next run; exiting scheduler");
                    break;
                }

                match perform_scheduled_backup(&config, args.always_backup) {
                    Ok(ScheduledBackup::Performed(summary)) => {
                        info!("backup created at {}", summary.path.display());
                        info!("backup size: {} bytes", summary.bytes);
                        if !summary.deleted.is_empty() {
                            info!("deleted {} older backup(s)", summary.deleted.len());
                        }
                    }
                    Ok(ScheduledBackup::Skipped) => {
                        info!(
                            "scheduled backup skipped because the Minecraft server does not appear to be running"
                        )
                    }
                    Err(error) => error!("scheduled backup failed: {error:#}"),
                }
            }
        }
    }

    Ok(())
}

fn install_ctrlc_handler() -> Result<Arc<AtomicBool>> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let signal = shutdown.clone();
    ctrlc::set_handler(move || {
        if !signal.swap(true, Ordering::SeqCst) {
            warn!("received interrupt signal");
        }
    })
    .context("failed to install Ctrl+C handler")?;

    Ok(shutdown)
}

fn wait_until(deadline: chrono::DateTime<Local>, shutdown: &AtomicBool) {
    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }

        let now = Local::now();
        if now >= deadline {
            break;
        }

        let remaining = (deadline - now)
            .to_std()
            .unwrap_or_else(|_| Duration::from_secs(0));
        std::thread::sleep(remaining.min(Duration::from_secs(1)));
    }
}

enum ScheduledBackup {
    Performed(backup::BackupSummary),
    Skipped,
}

fn perform_scheduled_backup(
    config: &backup::BackupConfig,
    always_backup: bool,
) -> Result<ScheduledBackup> {
    if !always_backup && !world_appears_running(&config.source)? {
        return Ok(ScheduledBackup::Skipped);
    }

    Ok(ScheduledBackup::Performed(perform_backup(config.clone())?))
}
