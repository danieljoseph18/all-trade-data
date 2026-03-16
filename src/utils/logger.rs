use chrono::{Local, Utc};
use fern::colors::{Color, ColoredLevelConfig};
use log::LevelFilter;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::sync::Mutex;

pub fn setup_logger() -> Result<(), fern::InitError> {
    let colors = ColoredLevelConfig::new()
        .info(Color::Green)
        .warn(Color::Yellow)
        .error(Color::Red)
        .debug(Color::Blue)
        .trace(Color::BrightBlack);

    fs::create_dir_all("logs")?;

    let current_date = Mutex::new(Utc::now().format("%Y-%m-%d").to_string());

    fern::Dispatch::new()
        .level(log::LevelFilter::Info)
        .level_for("h2", LevelFilter::Warn)
        .level_for("hyper", LevelFilter::Warn)
        .level_for("rustls", LevelFilter::Warn)
        .chain(
            fern::Dispatch::new()
                .format(move |out, message, record| {
                    out.finish(format_args!(
                        "[{}] [{}] [{}] - {}",
                        Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%Z"),
                        colors.color(record.level()),
                        record.target(),
                        message
                    ))
                })
                .chain(std::io::stdout()),
        )
        .chain(fern::Output::call(move |record| {
            let now_utc = Utc::now();
            let today = now_utc.format("%Y-%m-%d").to_string();

            let mut date_guard = current_date.lock().unwrap();
            if *date_guard != today {
                *date_guard = today.clone();
            }
            drop(date_guard);

            let log_path = format!("logs/app-{}.log", today);
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .expect("Failed to open log file");

            writeln!(
                file,
                "[{}] [{}] [{}] - {}",
                now_utc.format("%Y-%m-%dT%H:%M:%S%.3fZ"),
                record.level(),
                record.target(),
                record.args()
            )
            .expect("Failed to write to log file");
        }))
        .apply()?;

    Ok(())
}
