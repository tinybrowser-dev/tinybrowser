mod crash;
mod file;
mod format;

use std::fmt;
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::OnceLock;
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Level {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Level {
    type Err = ParseLevelError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "error" => Ok(Self::Error),
            "warn" | "warning" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            "trace" => Ok(Self::Trace),
            _ => Err(ParseLevelError),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseLevelError;

impl fmt::Display for ParseLevelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("unknown log level; expected error, warn, info, debug, or trace")
    }
}

impl std::error::Error for ParseLevelError {}

#[derive(Debug)]
pub enum InstallError {
    AlreadyInstalled,
    Open { path: PathBuf, source: io::Error },
    SpawnWriter(io::Error),
}

impl fmt::Display for InstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInstalled => formatter.write_str("logger already installed"),
            Self::Open { path, .. } => {
                write!(formatter, "cannot open log file {}", path.display())
            }
            Self::SpawnWriter(_) => formatter.write_str("cannot start the log writer"),
        }
    }
}

impl std::error::Error for InstallError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Open { source, .. } | Self::SpawnWriter(source) => Some(source),
            Self::AlreadyInstalled => None,
        }
    }
}

pub fn install(
    level: Level,
    file: Option<PathBuf>,
    clock: fn() -> SystemTime,
) -> Result<(), InstallError> {
    let _guard = INSTALL_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if LOGGER.get().is_some() {
        return Err(InstallError::AlreadyInstalled);
    }
    let logger = Logger::new(level, file, clock)?;
    LOGGER
        .set(logger)
        .map_err(|_| InstallError::AlreadyInstalled)?;
    crash::install(LOGGER.get().ok_or(InstallError::AlreadyInstalled)?);
    Ok(())
}

struct Logger {
    level: Level,
    clock: fn() -> SystemTime,
    file: Option<file::FileSink>,
}

impl Logger {
    fn new(
        level: Level,
        path: Option<PathBuf>,
        clock: fn() -> SystemTime,
    ) -> Result<Self, InstallError> {
        let file = path.map(file::FileSink::new).transpose()?;
        Ok(Self { level, clock, file })
    }

    fn enabled(&self, level: Level) -> bool {
        level <= self.level
    }

    fn log(&self, level: Level, target: &str, args: fmt::Arguments<'_>) {
        if self.enabled(level) {
            self.emit(format::record((self.clock)(), level, target, args));
        }
    }

    fn log_forwarded(&self, line: &str) {
        self.emit(format::forwarded(line));
    }

    fn emit(&self, line: String) {
        write_console(&line);
        if let Some(file) = &self.file {
            file.send(line);
        }
    }

    fn flush(&self) -> bool {
        self.file.as_ref().is_none_or(file::FileSink::flush)
    }
}

fn write_console(line: &str) {
    let stderr = io::stderr();
    let mut out = stderr.lock();
    let _ = out.write_all(line.as_bytes());
}

static LOGGER: OnceLock<Logger> = OnceLock::new();
static INSTALL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[must_use]
pub fn enabled(level: Level) -> bool {
    LOGGER.get().is_some_and(|logger| logger.enabled(level))
}

#[must_use]
pub fn level() -> Level {
    LOGGER.get().map_or(Level::Info, |logger| logger.level)
}

pub fn log(level: Level, target: &str, args: fmt::Arguments<'_>) {
    if let Some(logger) = LOGGER.get() {
        logger.log(level, target, args);
    }
}

pub fn log_forwarded(line: &str) {
    if let Some(logger) = LOGGER.get() {
        logger.log_forwarded(line);
    }
}

#[must_use]
pub fn flush() -> bool {
    LOGGER.get().is_none_or(Logger::flush)
}

#[macro_export]
macro_rules! error {
    (target: $target:expr, $($arg:tt)*) => {
        if $crate::enabled($crate::Level::Error) {
            $crate::log($crate::Level::Error, $target, format_args!($($arg)*))
        }
    };
    ($($arg:tt)*) => {
        if $crate::enabled($crate::Level::Error) {
            $crate::log($crate::Level::Error, module_path!(), format_args!($($arg)*))
        }
    };
}

#[macro_export]
macro_rules! warn {
    (target: $target:expr, $($arg:tt)*) => {
        if $crate::enabled($crate::Level::Warn) {
            $crate::log($crate::Level::Warn, $target, format_args!($($arg)*))
        }
    };
    ($($arg:tt)*) => {
        if $crate::enabled($crate::Level::Warn) {
            $crate::log($crate::Level::Warn, module_path!(), format_args!($($arg)*))
        }
    };
}

#[macro_export]
macro_rules! info {
    (target: $target:expr, $($arg:tt)*) => {
        if $crate::enabled($crate::Level::Info) {
            $crate::log($crate::Level::Info, $target, format_args!($($arg)*))
        }
    };
    ($($arg:tt)*) => {
        if $crate::enabled($crate::Level::Info) {
            $crate::log($crate::Level::Info, module_path!(), format_args!($($arg)*))
        }
    };
}

#[macro_export]
macro_rules! debug {
    (target: $target:expr, $($arg:tt)*) => {
        if $crate::enabled($crate::Level::Debug) {
            $crate::log($crate::Level::Debug, $target, format_args!($($arg)*))
        }
    };
    ($($arg:tt)*) => {
        if $crate::enabled($crate::Level::Debug) {
            $crate::log($crate::Level::Debug, module_path!(), format_args!($($arg)*))
        }
    };
}

#[macro_export]
macro_rules! trace {
    (target: $target:expr, $($arg:tt)*) => {
        if $crate::enabled($crate::Level::Trace) {
            $crate::log($crate::Level::Trace, $target, format_args!($($arg)*))
        }
    };
    ($($arg:tt)*) => {
        if $crate::enabled($crate::Level::Trace) {
            $crate::log($crate::Level::Trace, module_path!(), format_args!($($arg)*))
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_order_from_most_to_least_severe() {
        assert!(Level::Error < Level::Warn);
        assert!(Level::Warn < Level::Info);
        assert!(Level::Info < Level::Debug);
        assert!(Level::Debug < Level::Trace);
    }

    #[test]
    fn levels_parse_and_display() {
        assert_eq!("error".parse::<Level>(), Ok(Level::Error));
        assert_eq!("WARNING".parse::<Level>(), Ok(Level::Warn));
        assert_eq!("Info".parse::<Level>(), Ok(Level::Info));
        assert_eq!("debug".parse::<Level>(), Ok(Level::Debug));
        assert_eq!("trace".parse::<Level>(), Ok(Level::Trace));
        assert!("verbose".parse::<Level>().is_err());
        assert_eq!(Level::Warn.to_string(), "WARN");
    }

    #[test]
    fn threshold_filters_less_severe_records() {
        let logger = Logger::new(Level::Info, None, SystemTime::now).expect("logger");
        assert!(logger.enabled(Level::Error));
        assert!(logger.enabled(Level::Info));
        assert!(!logger.enabled(Level::Debug));
    }

    #[test]
    fn unopenable_file_reports_its_path() {
        let path = PathBuf::from("/proc/tinybrowser-cannot-exist/log");
        let logger = Logger::new(Level::Info, Some(path.clone()), SystemTime::now);
        assert!(matches!(
            logger,
            Err(InstallError::Open { path: failed, .. }) if failed == path
        ));
    }

    #[test]
    fn records_stamp_with_the_injected_clock() {
        let dir =
            std::env::temp_dir().join(format!("tinybrowser-log-clock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("log");
        let logger = Logger::new(Level::Info, Some(path.clone()), || SystemTime::UNIX_EPOCH)
            .expect("logger");
        logger.log(Level::Info, "test", format_args!("stamped"));
        assert!(logger.flush());
        let contents = std::fs::read_to_string(&path).expect("read");
        assert!(
            contents.contains("1970-01-01T00:00:00.000Z INFO test stamped"),
            "{contents}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn disabled_macros_do_not_evaluate_arguments() {
        let mut evaluated = false;
        crate::info!(target: "test", "{}", {
            evaluated = true;
            "value"
        });
        assert!(!evaluated, "a filtered macro must not run its arguments");
    }
}
