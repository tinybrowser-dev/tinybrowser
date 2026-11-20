use std::collections::{BTreeSet, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::Path;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use wpt_report::Outcome;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stale {
    Died,
    TimedOut,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
enum Record {
    Start { id: String },
    Done { outcome: Outcome },
}

#[derive(Default)]
pub struct Resumed {
    pub done: HashMap<String, Outcome>,
    pub stale: BTreeSet<String>,
}

pub struct Journal {
    file: Mutex<File>,
}

impl Journal {
    pub fn open(path: &Path) -> io::Result<(Self, Resumed)> {
        let resumed = read(path)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)?;
        Ok((
            Self {
                file: Mutex::new(file),
            },
            resumed,
        ))
    }

    pub fn start(&self, id: &str) {
        self.append(&Record::Start { id: id.to_owned() });
    }

    pub fn finish(&self, outcome: &Outcome) {
        self.append(&Record::Done {
            outcome: outcome.clone(),
        });
    }

    fn append(&self, record: &Record) {
        let Ok(mut line) = serde_json::to_vec(record) else {
            return;
        };
        line.push(b'\n');
        let mut file = match self.file.lock() {
            Ok(file) => file,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Err(error) = file.write_all(&line) {
            eprintln!("wpt-harness: cannot write the journal: {error}");
        }
    }
}

fn read(path: &Path) -> io::Result<Resumed> {
    let mut resumed = Resumed::default();
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(resumed),
        Err(error) => return Err(error),
    };
    let mut started = BTreeSet::new();
    for line in BufReader::new(file).lines() {
        let line = line?;
        let Ok(record) = serde_json::from_str::<Record>(&line) else {
            continue;
        };
        match record {
            Record::Start { id } => {
                started.insert(id);
            }
            Record::Done { outcome } => {
                resumed.done.insert(outcome.id.clone(), outcome);
            }
        }
    }
    resumed.stale = started
        .into_iter()
        .filter(|id| !resumed.done.contains_key(id))
        .collect();
    Ok(resumed)
}

#[must_use]
pub fn stale_message(stale: Stale) -> &'static str {
    match stale {
        Stale::Died => "the harness process died while this file ran",
        Stale::TimedOut => "the harness attempt timed out while this file ran",
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use wpt_report::{Outcome, Status};

    use super::{Journal, Stale, stale_message};

    fn scratch(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "wpt-harness-journal-{name}-{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn outcome(id: &str, status: Status) -> Outcome {
        Outcome {
            id: id.into(),
            status,
            message: None,
            duration: Duration::from_millis(7),
            subtests: Vec::new(),
            testharness: true,
        }
    }

    #[test]
    fn a_finished_file_is_resumed_with_its_verdict() {
        let path = scratch("finished");
        {
            let (journal, resumed) = Journal::open(&path).expect("journal");
            assert!(resumed.done.is_empty());
            journal.start("/a.html");
            journal.finish(&outcome("/a.html", Status::Fail));
        }
        let (_, resumed) = Journal::open(&path).expect("reopened");
        assert_eq!(resumed.done["/a.html"].status, Status::Fail);
        assert_eq!(resumed.done["/a.html"].duration, Duration::from_millis(7));
        assert!(resumed.stale.is_empty());
        std::fs::remove_file(&path).expect("removed");
    }

    #[test]
    fn an_unfinished_file_is_stale_once() {
        let path = scratch("stale");
        {
            let (journal, _) = Journal::open(&path).expect("journal");
            journal.start("/b.html");
        }
        let (journal, resumed) = Journal::open(&path).expect("reopened");
        assert_eq!(
            resumed.stale.iter().cloned().collect::<Vec<_>>(),
            ["/b.html".to_owned()]
        );
        assert_eq!(
            stale_message(Stale::TimedOut),
            "the harness attempt timed out while this file ran"
        );
        journal.finish(&outcome("/b.html", Status::Timeout));
        drop(journal);
        let (_, resumed) = Journal::open(&path).expect("third attempt");
        assert!(resumed.stale.is_empty());
        assert_eq!(resumed.done["/b.html"].status, Status::Timeout);
        std::fs::remove_file(&path).expect("removed");
    }

    #[test]
    fn a_torn_line_is_ignored() {
        let path = scratch("torn");
        std::fs::write(
            &path,
            "{\"event\":\"start\",\"id\":\"/a.html\"}\n{\"event\":\"done\",\"outc",
        )
        .expect("written");
        let (_, resumed) = Journal::open(&path).expect("opened");
        assert_eq!(
            resumed.stale.iter().cloned().collect::<Vec<_>>(),
            ["/a.html".to_owned()]
        );
        assert!(resumed.done.is_empty());
        std::fs::remove_file(&path).expect("removed");
    }
}
