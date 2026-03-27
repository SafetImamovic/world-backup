use anyhow::Result;
use env_logger::Env;
use log::LevelFilter;

pub fn init_logging(verbose: u8) -> Result<()> {
    let filter = match verbose {
        0 => LevelFilter::Info,
        1 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    };

    let mut builder = env_logger::Builder::from_env(Env::default().default_filter_or("info"));
    builder.filter_level(filter);
    builder.format_timestamp_secs();
    builder.try_init()?;
    Ok(())
}
