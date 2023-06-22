// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::{FromArgs, TopLevelCommand};
use chrono::{DateTime, Local};
use chrono_english::{parse_date_string, Dialect};
use diagnostics_data::Severity;
use fidl_fuchsia_diagnostics::LogInterestSelector;
use std::{ops::Deref, time::Duration};
pub mod filter;
pub mod log_formatter;
pub mod log_socket_stream;

// Subcommand for ffx log (either watch or dump).
#[derive(FromArgs, Clone, PartialEq, Debug)]
#[argh(subcommand)]
pub enum LogSubCommand {
    Watch(WatchCommand),
    Dump(DumpCommand),
}

#[derive(FromArgs, Clone, PartialEq, Debug)]
/// Watches for and prints logs from a target. Default if no sub-command is specified.
#[argh(subcommand, name = "watch")]
pub struct WatchCommand {}

#[derive(FromArgs, Clone, PartialEq, Debug)]
/// Dumps all log from a given target's session.
#[argh(subcommand, name = "dump")]
pub struct DumpCommand {
    /// A specifier indicating which session you'd like to retrieve logs for.
    /// For example, providing ~1 retrieves the most-recent session,
    /// ~2 the second-most-recent, and so on.
    /// Defaults to the most recent session.
    #[argh(positional, default = "SessionSpec::Relative(0)", from_str_fn(parse_session_spec))]
    pub session: SessionSpec,
}

pub fn parse_time(value: &str) -> Result<DetailedDateTime, String> {
    let d = parse_date_string(value, Local::now(), Dialect::Us)
        .map(|time| DetailedDateTime { time, is_now: value == "now" })
        .map_err(|e| format!("invalid date string: {}", e));
    d
}

/// Specifies the session to subscribe to.
/// This lets you get logs based on a specific absolute timestamp
/// or relative time.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionSpec {
    TimestampNanos(u64),
    Relative(u32),
}

/// Parses a duration from a string. The input is in seconds
/// and the output is a Rust duration.
pub fn parse_seconds_string_as_duration(value: &str) -> Result<Duration, String> {
    Ok(Duration::from_secs(
        value.parse().map_err(|e| format!("value '{}' is not a number: {}", value, e))?,
    ))
}

/// Parses a session spec from a string. The session spec is a number
/// that can be either a relative timestamp or an absolute timestamp.
/// Values starting with ~ are parsed as relative timestamps.
/// Values starting without ~ are parsed as absolute timestamps.
/// All timestamps are specified in nanoseconds.
pub fn parse_session_spec(value: &str) -> Result<SessionSpec, String> {
    if value.is_empty() {
        return Err(String::from("session identifier cannot be empty"));
    }

    if value == "0" {
        return Err(String::from("'0' is not a valid session specifier: use ~1 for the most recent session in `dump` mode."));
    }

    let split = value.split_once('~');
    if let Some((_, val)) = split {
        Ok(SessionSpec::Relative(val.parse().map_err(|e| {
            format!(
                "previous session provided with '~' but could not parse the rest as a number: {}",
                e
            )
        })?))
    } else {
        Ok(SessionSpec::TimestampNanos(value.parse().map_err(|e| {
            format!("session identifier was provided, but could not be parsed as a number: {}", e)
        })?))
    }
}

// Time format for displaying logs
#[derive(Clone, Debug, PartialEq)]
pub enum TimeFormat {
    // UTC time
    Utc,
    // Local time
    Local,
    // Monotonic time
    Monotonic,
}

impl std::str::FromStr for TimeFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lower = s.to_ascii_lowercase();
        match lower.as_str() {
            "local" => Ok(TimeFormat::Local),
            "utc" => Ok(TimeFormat::Utc),
            "monotonic" => Ok(TimeFormat::Monotonic),
            _ => Err(format!(
                "'{}' is not a valid value: must be one of 'local', 'utc', 'monotonic'",
                s
            )),
        }
    }
}

/// Date/time structure containing a "now"
/// field, set if it should be interpreted as the
/// current time (used to call Subscribe instead of SnapshotThenSubscribe).
#[derive(PartialEq, Clone, Debug)]
pub struct DetailedDateTime {
    /// The absolute timestamp as specified by the user
    /// or the current timestamp if 'now' is specified.
    pub time: DateTime<Local>,
    /// Whether or not the DateTime was "now".
    /// If the DateTime is "now", logs will be collected in subscribe
    /// mode, instead of SnapshotThenSubscribe.
    pub is_now: bool,
}

impl Deref for DetailedDateTime {
    type Target = DateTime<Local>;

    fn deref(&self) -> &Self::Target {
        &self.time
    }
}

#[derive(FromArgs, Clone, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "log",
    description = "Display logs from a target device",
    note = "Logs are retrieve from the target at the moment this command is called.

You may see some additional information attached to the log line:

- `dropped=N`: this means that N logs attributed to the component were dropped when the component
  wrote to the log socket. This can happen when archivist cannot keep up with the rate of logs being
  emitted by the component and the component filled the log socket buffer in the kernel.

- `rolled=N`: this means that N logs rolled out from the archivist buffer and ffx never saw them.
  This can happen when more logs are being ingested by the archivist across all components and the
  ffx couldn't retrieve them fast enough.

Symbolization is performed in the background using the symbolizer host tool. You can pass
additional arguments to the symbolizer tool (for example, to add a remote symbol server) using:
  $ ffx config set proactive_log.symbolize.extra_args \"--symbol-server gs://some-url/path --symbol-server gs://some-other-url/path ...\"

To learn more about configuring the log viewer, visit https://fuchsia.dev/fuchsia-src/development/tools/ffx/commands/log",
    example = "\
Dump the most recent logs and stream new ones as they happen:
  $ ffx log

Stream new logs starting from the current time, filtering for severity of at least \"WARN\":
  $ ffx log --severity warn --since now

Stream logs where the source moniker, component url and message do not include \"sys\":
  $ ffx log --exclude sys

Stream ERROR logs with source moniker, component url or message containing either
\"netstack\" or \"remote-control.cm\", but not containing \"sys\":
  $ ffx log --severity error --filter netstack --filter remote-control.cm --exclude sys

Dump all available logs where the source moniker, component url, or message contains
\"remote-control\":
  $ ffx log --filter remote-control dump

Dump all logs from the last 30 minutes logged before 5 minutes ago:
  $ ffx log --since \"30m ago\" --until \"5m ago\" dump

Enable DEBUG logs from the \"core/audio\" component while logs are streaming:
  $ ffx log --select core/audio#DEBUG"
)]
pub struct LogCommand {
    #[argh(subcommand)]
    pub sub_command: Option<LogSubCommand>,

    /// filter for a string in either the message, component or url.
    /// May be repeated.
    #[argh(option)]
    pub filter: Vec<String>,

    /// filter for a component moniker.
    /// May be repeated.
    #[argh(option)]
    pub moniker: Vec<String>,

    /// exclude a string in either the message, component or url.
    /// May be repeated.
    #[argh(option)]
    pub exclude: Vec<String>,

    /// filter for only logs with a given tag. May be repeated.
    #[argh(option)]
    pub tags: Vec<String>,

    /// exclude logs with a given tag. May be repeated.
    #[argh(option)]
    pub exclude_tags: Vec<String>,

    /// set the minimum severity. Accepted values (from lower to higher) are: trace, debug, info,
    /// warn (or warning), error, fatal. This field is case insensitive.
    #[argh(option, default = "Severity::Info")]
    pub severity: Severity,

    /// outputs only kernel logs.
    #[argh(switch)]
    pub kernel: bool,

    /// show only logs after a certain time
    #[argh(option, from_str_fn(parse_time))]
    pub since: Option<DetailedDateTime>,

    /// show only logs after a certain time (as a monotonic
    /// timestamp: seconds from the target's boot time).
    #[argh(option, from_str_fn(parse_seconds_string_as_duration))]
    pub since_monotonic: Option<Duration>,

    /// show only logs until a certain time
    #[argh(option, from_str_fn(parse_time))]
    pub until: Option<DetailedDateTime>,

    /// show only logs until a certain time (as a monotonic
    /// timestamp: seconds since the target's boot time).
    #[argh(option, from_str_fn(parse_seconds_string_as_duration))]
    pub until_monotonic: Option<Duration>,

    /// hide the tag field from output (does not exclude any log messages)
    #[argh(switch)]
    pub hide_tags: bool,

    /// hide the file and line number field from output (does not exclude any log messages)
    #[argh(switch)]
    pub hide_file: bool,

    /// disable coloring logs according to severity.
    /// Note that you can permanently disable this with
    /// `ffx config set log_cmd.color false`
    #[argh(switch)]
    pub no_color: bool,

    /// shows process-id and thread-id in log output
    #[argh(switch)]
    pub show_metadata: bool,

    /// shows the full moniker in log output. By default this is false and only the last segment
    /// of the moniker is printed.
    #[argh(switch)]
    pub show_full_moniker: bool,

    /// how to display log timestamps.
    /// Options are "utc", "local", or "monotonic" (i.e. nanos since target boot).
    /// Default is monotonic.
    #[argh(option, default = "TimeFormat::Monotonic")]
    pub clock: TimeFormat,

    /// if provided, logs will not be symbolized
    #[argh(switch)]
    pub raw: bool,

    /// configure the log settings on the target device for components matching
    /// the given selector. This modifies the minimum log severity level emitted
    /// by components during the logging session.
    /// Specify using the format <component-selector>#<log-level>, with level
    /// as one of FATAL|ERROR|WARN|INFO|DEBUG|TRACE.
    /// May be repeated.
    #[argh(option, from_str_fn(log_interest_selector))]
    pub select: Vec<LogInterestSelector>,

    /// if provided, overrides the default log spam list path that's optionally
    /// specified in FFX config under the "log_cmd.spam_filepath" key.
    #[argh(option)]
    pub spam_list_path: Option<String>,

    /// if set, disable log spam filtering
    #[argh(switch)]
    pub disable_spam_filter: bool,

    /// if set and spam filter is enabled, spams will be displayed and highlighted
    #[argh(switch)]
    pub enable_spam_highlight: bool,
    /// filters by pid
    #[argh(option)]
    pub pid: Option<u64>,
    /// filters by tid
    #[argh(option)]
    pub tid: Option<u64>,
}

impl Default for LogCommand {
    fn default() -> Self {
        LogCommand {
            filter: vec![],
            moniker: vec![],
            exclude: vec![],
            tags: vec![],
            exclude_tags: vec![],
            hide_tags: false,
            hide_file: false,
            clock: TimeFormat::Monotonic,
            no_color: false,
            kernel: false,
            severity: Severity::Info,
            show_metadata: false,
            raw: false,
            since: None,
            since_monotonic: None,
            until: None,
            until_monotonic: None,
            sub_command: None,
            select: vec![],
            show_full_moniker: false,
            spam_list_path: None,
            disable_spam_filter: false,
            enable_spam_highlight: false,
            pid: None,
            tid: None,
        }
    }
}

impl TopLevelCommand for LogCommand {}

fn log_interest_selector(s: &str) -> Result<LogInterestSelector, String> {
    selectors::parse_log_interest_selector(s).map_err(|s| s.to_string())
}

#[cfg(test)]
mod test {
    use super::*;
    use selectors::parse_log_interest_selector;

    #[test]
    fn test_parse_selector() {
        assert_eq!(
            log_interest_selector("core/audio#DEBUG").unwrap(),
            parse_log_interest_selector("core/audio#DEBUG").unwrap()
        );
    }

    #[test]
    fn test_parse_time() {
        assert_eq!(parse_time("now").unwrap().is_now, true);
        let date_string = "04/20/2020";
        let res = parse_time(date_string).unwrap();
        assert_eq!(res.is_now, false);
        assert_eq!(
            res.date(),
            parse_date_string(date_string, Local::now(), Dialect::Us).unwrap().date()
        );
    }

    #[test]
    fn test_session_spec_non_zero() {
        assert_eq!(parse_session_spec("~1").unwrap(), SessionSpec::Relative(1));
        assert_eq!(parse_session_spec("~15").unwrap(), SessionSpec::Relative(15));
    }

    #[test]
    fn test_session_spec_absolute() {
        assert_eq!(parse_session_spec("1234567").unwrap(), SessionSpec::TimestampNanos(1234567));
    }

    #[test]
    fn test_session_spec_error() {
        assert!(parse_session_spec("~abc").is_err());
        assert!(parse_session_spec("abc").is_err());
    }
}
