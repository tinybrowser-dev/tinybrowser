use std::fmt::{self, Write as _};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::Level;

pub(crate) fn record(
    time: SystemTime,
    level: Level,
    target: &str,
    args: fmt::Arguments<'_>,
) -> String {
    let mut line = timestamp(time);
    line.push(' ');
    line.push_str(level.as_str());
    line.push(' ');
    let _ = write!(Escaped(&mut line), "{target}");
    line.push(' ');
    let _ = write!(Escaped(&mut line), "{args}");
    line.push('\n');
    line
}

pub(crate) fn forwarded(line: &str) -> String {
    let mut out = String::with_capacity(line.len() + 1);
    let _ = write!(Escaped(&mut out), "{}", line.trim_end_matches(['\r', '\n']));
    out.push('\n');
    out
}

pub(crate) fn timestamp(time: SystemTime) -> String {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = since_epoch.as_secs();
    let millis = since_epoch.subsec_millis();
    let days = seconds / 86_400;
    let day_seconds = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = day_seconds / 3_600;
    let minute = (day_seconds % 3_600) / 60;
    let second = day_seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn civil_from_days(days: u64) -> (u64, u64, u64) {
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

struct Escaped<'a>(&'a mut String);

impl fmt::Write for Escaped<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        for c in value.chars() {
            match c {
                '\n' => self.0.push_str("\\n"),
                '\r' => self.0.push_str("\\r"),
                _ => self.0.push(c),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn timestamps_are_rfc3339_utc() {
        let epoch = SystemTime::UNIX_EPOCH;
        assert_eq!(timestamp(epoch), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            timestamp(epoch + Duration::from_millis(1_789_171_200_123)),
            "2026-09-12T00:00:00.123Z"
        );
    }

    #[test]
    fn records_render_one_exact_line() {
        assert_eq!(
            record(
                SystemTime::UNIX_EPOCH,
                Level::Info,
                "browser::tab",
                format_args!("created tab")
            ),
            "1970-01-01T00:00:00.000Z INFO browser::tab created tab\n"
        );
    }

    #[test]
    fn line_breaks_in_messages_are_escaped() {
        assert_eq!(
            record(
                SystemTime::UNIX_EPOCH,
                Level::Error,
                "cli",
                format_args!("bad \"value\" a=b\nnext\rline")
            ),
            "1970-01-01T00:00:00.000Z ERROR cli bad \"value\" a=b\\nnext\\rline\n"
        );
    }

    #[test]
    fn line_breaks_in_targets_are_escaped() {
        assert_eq!(
            record(
                SystemTime::UNIX_EPOCH,
                Level::Info,
                "a\nb",
                format_args!("m")
            ),
            "1970-01-01T00:00:00.000Z INFO a\\nb m\n"
        );
    }

    #[test]
    fn forwarded_lines_trim_the_terminator_and_escape_cr() {
        assert_eq!(forwarded("child stderr\r\n"), "child stderr\n");
        assert_eq!(forwarded("one\rline\n"), "one\\rline\n");
    }
}
