use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;
use std::time::{Duration, Instant};

const QUEUE_CAPACITY: usize = 1024;
pub(crate) const ROTATE_BYTES: u64 = 8 * 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(2);

enum Message {
    Line(String),
    Flush(mpsc::Sender<()>),
}

pub(crate) struct FileSink {
    tx: SyncSender<Message>,
    path: PathBuf,
}

impl FileSink {
    pub(crate) fn new(path: PathBuf) -> Result<Self, crate::InstallError> {
        let file = open(&path).map_err(|source| crate::InstallError::Open {
            path: path.clone(),
            source,
        })?;
        let (tx, rx) = mpsc::sync_channel(QUEUE_CAPACITY);
        let writer_path = path.clone();
        thread::Builder::new()
            .name("logging-file".to_owned())
            .spawn(move || run(&rx, &writer_path, file, ROTATE_BYTES))
            .map_err(crate::InstallError::SpawnWriter)?;
        Ok(Self { tx, path })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn send(&self, line: String) {
        let _ = self.tx.try_send(Message::Line(line));
    }

    pub(crate) fn flush(&self) -> bool {
        let (ack_tx, ack_rx) = mpsc::channel();
        let mut message = Message::Flush(ack_tx);
        let deadline = Instant::now() + TIMEOUT;
        loop {
            match self.tx.try_send(message) {
                Ok(()) => return ack_rx.recv_timeout(TIMEOUT).is_ok(),
                Err(TrySendError::Full(returned)) => {
                    if Instant::now() >= deadline {
                        return false;
                    }
                    message = returned;
                    thread::sleep(Duration::from_millis(1));
                }
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
    }
}

fn run(rx: &Receiver<Message>, path: &Path, mut file: File, rotate_at: u64) {
    let mut written = file.metadata().map_or(0, |meta| meta.len());
    while let Ok(message) = rx.recv() {
        match message {
            Message::Line(line) => written = append(&mut file, path, &line, written, rotate_at),
            Message::Flush(ack) => {
                let _ = ack.send(());
            }
        }
    }
}

fn append(file: &mut File, path: &Path, line: &str, written: u64, rotate_at: u64) -> u64 {
    let written = if crosses_cap(written, line, rotate_at) && rotate(file, path).is_some() {
        0
    } else {
        written
    };
    if file.write_all(line.as_bytes()).is_err() {
        return file.metadata().map_or(written, |meta| meta.len());
    }
    written.saturating_add(len(line))
}

fn crosses_cap(written: u64, line: &str, rotate_at: u64) -> bool {
    written
        .checked_add(len(line))
        .is_none_or(|total| total > rotate_at)
}

fn rotate(file: &mut File, path: &Path) -> Option<()> {
    let backup = backup_path(path);
    let _ = fs::remove_file(&backup);
    fs::rename(path, &backup).ok()?;
    if let Ok(fresh) = open(path) {
        *file = fresh;
        Some(())
    } else {
        let _ = fs::rename(&backup, path);
        None
    }
}

fn len(line: &str) -> u64 {
    u64::try_from(line.len()).unwrap_or(u64::MAX)
}

fn backup_path(path: &Path) -> PathBuf {
    let mut backup = path.as_os_str().to_owned();
    backup.push(".1");
    PathBuf::from(backup)
}

pub(crate) fn open(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        let _ = restrict_dir(parent);
    }
    let mut options = File::options();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    let _ = restrict_file(path);
    Ok(file)
}

#[cfg(unix)]
fn restrict_dir(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
fn restrict_dir(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_file(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_file(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT: AtomicU32 = AtomicU32::new(0);

    fn temp_path(tag: &str) -> (PathBuf, PathBuf) {
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "tinybrowser-logging-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(format!("{tag}.log"));
        (dir, path)
    }

    fn line(text: &str) -> String {
        format!("{text}\n")
    }

    fn flush(tx: &SyncSender<Message>) {
        let (ack_tx, ack_rx) = mpsc::channel();
        tx.send(Message::Flush(ack_tx)).expect("flush");
        ack_rx.recv_timeout(TIMEOUT).expect("flush ack");
    }

    #[test]
    fn writes_queued_lines_and_flushes() {
        let (dir, path) = temp_path("flush");
        let sink = FileSink::new(path.clone()).expect("sink");
        sink.send(line("one"));
        sink.send(line("two"));
        assert!(sink.flush());
        assert_eq!(fs::read_to_string(&path).expect("read"), "one\ntwo\n");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rotates_once_at_the_cap() {
        let (dir, path) = temp_path("rotate");
        let file = open(&path).expect("file");
        let (tx, rx) = mpsc::sync_channel(8);
        let writer_path = path.clone();
        let writer = thread::spawn(move || run(&rx, &writer_path, file, 16));

        assert!(tx.try_send(Message::Line(line("aaaaaaaaaa"))).is_ok());
        flush(&tx);
        assert!(tx.try_send(Message::Line(line("bbbbbbbbbb"))).is_ok());
        flush(&tx);
        drop(tx);
        writer.join().expect("writer");

        assert_eq!(
            fs::read_to_string(backup_path(&path)).expect("backup"),
            "aaaaaaaaaa\n"
        );
        assert_eq!(fs::read_to_string(&path).expect("live"), "bbbbbbbbbb\n");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_archive_keeps_writing_and_counting() {
        let (dir, path) = temp_path("rotate-fail");
        let file = open(&path).expect("file");
        let (tx, rx) = mpsc::sync_channel(8);
        let writer_path = path.clone();
        let writer = thread::spawn(move || run(&rx, &writer_path, file, 16));

        assert!(tx.try_send(Message::Line(line("aaaaaaaaaa"))).is_ok());
        flush(&tx);
        let backup = backup_path(&path);
        fs::create_dir(&backup).expect("backup dir");
        fs::write(backup.join("keep"), "x").expect("backup file");
        assert!(tx.try_send(Message::Line(line("bbbbbbbbbb"))).is_ok());
        flush(&tx);
        assert_eq!(
            fs::read_to_string(&path).expect("live"),
            "aaaaaaaaaa\nbbbbbbbbbb\n",
            "a failed archive must not lose lines"
        );

        fs::remove_dir_all(&backup).expect("unblock");
        assert!(tx.try_send(Message::Line(line("cccccccccc"))).is_ok());
        flush(&tx);
        drop(tx);
        writer.join().expect("writer");
        assert_eq!(fs::read_to_string(&path).expect("live"), "cccccccccc\n");
        assert!(
            fs::read_to_string(&backup)
                .expect("backup")
                .contains("aaaaaaaaaa"),
            "archived line missing"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn log_files_are_user_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let (dir, path) = temp_path("permissions");
        let sink = FileSink::new(path.clone()).expect("sink");
        sink.send(line("one"));
        assert!(sink.flush());
        let file_mode = fs::metadata(&path).expect("file").permissions().mode() & 0o777;
        let dir_mode = fs::metadata(&dir).expect("dir").permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "log file");
        assert_eq!(dir_mode, 0o700, "log directory");
        let _ = fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn rotated_log_files_are_user_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let (dir, path) = temp_path("rotated-permissions");
        let file = open(&path).expect("file");
        let (tx, rx) = mpsc::sync_channel(8);
        let writer_path = path.clone();
        let writer = thread::spawn(move || run(&rx, &writer_path, file, 1));

        assert!(tx.try_send(Message::Line(line("rotate"))).is_ok());
        flush(&tx);
        drop(tx);
        writer.join().expect("writer");

        let mode = fs::metadata(&path).expect("live").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "rotated log file");
        let _ = fs::remove_dir_all(dir);
    }
}
