//! Log records and the line format they are parsed from.

use std::fmt;

/// Severity of a log record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
    /// The line did not follow the expected format.
    Unknown,
}

impl Level {
    /// Every level, most severe first.
    pub const ALL: [Level; 6] = [
        Level::Error,
        Level::Warn,
        Level::Info,
        Level::Debug,
        Level::Trace,
        Level::Unknown,
    ];

    /// Parses a level name as written in a log line or a filter, ignoring case.
    pub fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "trace" => Some(Level::Trace),
            "debug" => Some(Level::Debug),
            "info" => Some(Level::Info),
            "warn" | "warning" => Some(Level::Warn),
            "error" | "err" => Some(Level::Error),
            "unknown" => Some(Level::Unknown),
            _ => None,
        }
    }

    /// The level as shown in tables, e.g. `WARN`.
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Trace => "TRACE",
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
            Level::Unknown => "?",
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One line of a log file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogRecord {
    /// 1-based line number in the source file.
    pub line: usize,
    pub timestamp: String,
    pub level: Level,
    pub component: String,
    pub message: String,
}

/// Parses `<timestamp> <LEVEL> <component>: <message>`.
///
/// A line in any other shape is kept whole as the message of a record with
/// [`Level::Unknown`], so nothing in the file is hidden from the user.
pub fn parse_line(line: usize, text: &str) -> LogRecord {
    let unknown = || LogRecord {
        line,
        timestamp: String::new(),
        level: Level::Unknown,
        component: String::new(),
        message: text.to_owned(),
    };
    let mut parts = text.splitn(3, ' ');
    let (Some(timestamp), Some(level), Some(rest)) = (parts.next(), parts.next(), parts.next())
    else {
        return unknown();
    };
    let Some(level) = Level::parse(level).filter(|level| *level != Level::Unknown) else {
        return unknown();
    };
    let Some((component, message)) = rest.split_once(": ") else {
        return unknown();
    };
    if timestamp.is_empty() || component.is_empty() || component.contains(' ') {
        return unknown();
    }
    LogRecord {
        line,
        timestamp: timestamp.to_owned(),
        level,
        component: component.to_owned(),
        message: message.to_owned(),
    }
}

/// Parses a whole file. Blank lines are skipped; line numbers still count them.
pub fn parse_log(text: &str) -> Vec<LogRecord> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| parse_line(index + 1, line))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_formed_line_is_split_into_fields() {
        assert_eq!(
            parse_line(
                7,
                "2026-11-16T09:00:02Z WARN auth.session: token expires in 5 min: user=42"
            ),
            LogRecord {
                line: 7,
                timestamp: "2026-11-16T09:00:02Z".into(),
                level: Level::Warn,
                component: "auth.session".into(),
                message: "token expires in 5 min: user=42".into(),
            }
        );
    }

    #[test]
    fn level_names_are_case_insensitive_with_common_aliases() {
        for (text, level) in [
            ("error", Level::Error),
            ("ERR", Level::Error),
            ("Warning", Level::Warn),
            ("info", Level::Info),
        ] {
            assert_eq!(
                parse_line(1, &format!("t {text} c: m")).level,
                level,
                "{text}"
            );
        }
    }

    #[test]
    fn malformed_lines_are_kept_whole_as_unknown() {
        for text in [
            "just some text",
            "2026-11-16 FATAL core: unknown level name",
            "2026-11-16 INFO no-colon-here",
            "2026-11-16 INFO two words: message",
            "2026-11-16 UNKNOWN core: the placeholder level is not a real level",
            "    at com.example.Stack.trace(Stack.java:1)",
        ] {
            let record = parse_line(3, text);
            assert_eq!(record.level, Level::Unknown, "{text}");
            assert_eq!(record.message, text);
            assert_eq!(
                (record.timestamp.as_str(), record.component.as_str()),
                ("", "")
            );
        }
    }

    #[test]
    fn blank_lines_are_skipped_but_counted() {
        let records = parse_log("t INFO a: one\n\n   \nt ERROR b: four\n");
        assert_eq!(
            records
                .iter()
                .map(|record| (record.line, record.level))
                .collect::<Vec<_>>(),
            [(1, Level::Info), (4, Level::Error)]
        );
        assert!(parse_log("").is_empty());
    }
}
