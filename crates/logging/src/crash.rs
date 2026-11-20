use std::io::Write as _;

pub(crate) fn install(logger: &'static crate::Logger) {
    std::panic::set_hook(Box::new(move |info| record(logger, info)));
}

fn record(logger: &crate::Logger, info: &std::panic::PanicHookInfo<'_>) {
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload");
    let location = info.location().map_or_else(
        || "unknown location".to_owned(),
        |location| {
            format!(
                "{}:{}:{}",
                location.file(),
                location.line(),
                location.column()
            )
        },
    );
    let line = crate::format::record(
        (logger.clock)(),
        crate::Level::Error,
        "panic",
        format_args!("{location} {message}"),
    );
    crate::write_console(&line);
    if let Some(path) = logger.file.as_ref().map(crate::file::FileSink::path) {
        append(path, &line);
    }
}

fn append(path: &std::path::Path, line: &str) {
    if let Ok(mut file) = crate::file::open(path) {
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::append;
    use std::path::{Path, PathBuf};

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "tinybrowser-{prefix}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).expect("temp dir");
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn appends_to_an_existing_file() {
        let dir = TempDir::new("crash");
        let path = dir.path().join("log");
        std::fs::write(&path, "existing\n").expect("seed");
        append(&path, "crash line\n");
        append(&path, "second line\n");
        let contents = std::fs::read_to_string(&path).expect("read");
        assert_eq!(contents, "existing\ncrash line\nsecond line\n");
    }
}
