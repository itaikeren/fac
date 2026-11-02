use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

/// Initialise file logging. Returns the guard that must live until exit.
pub fn init() -> Result<WorkerGuard> {
    let path = log_file_path()?;
    let dir = path
        .parent()
        .ok_or_else(|| anyhow!("log file path has no parent directory: {}", path.display()))?;
    let file_name = path
        .file_name()
        .ok_or_else(|| anyhow!("log file path has no file name: {}", path.display()))?;
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating log directory {}", dir.display()))?;

    let filter = match std::env::var("FAC_LOG") {
        Ok(spec) => EnvFilter::try_new(spec).context("parsing FAC_LOG")?,
        Err(_) => EnvFilter::new("info"),
    };

    let appender = tracing_appender::rolling::never(dir, file_name);
    let (writer, guard) = tracing_appender::non_blocking(appender);
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_writer(writer)
        .try_init()
        .map_err(|err| anyhow!("installing tracing subscriber: {err}"))?;
    Ok(guard)
}

/// The log file: `FAC_LOG_FILE`, else `~/Library/Logs/fac/fac.log`.
pub fn log_file_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("FAC_LOG_FILE") {
        return Ok(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow!("HOME is not set"))?;
    Ok(PathBuf::from(home).join("Library/Logs/fac/fac.log"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_info_lines_to_the_log_file() {
        let path = std::env::temp_dir().join(format!("fac-log-test-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        // SAFETY: this is the only test that touches the environment.
        unsafe { std::env::set_var("FAC_LOG_FILE", &path) };

        let guard = init().expect("init succeeds");
        tracing::info!("hello from the logging test");
        drop(guard);

        let contents = std::fs::read_to_string(&path).expect("log file exists");
        assert!(
            contents.contains("hello from the logging test"),
            "{contents}"
        );
        let _ = std::fs::remove_file(&path);
    }
}
