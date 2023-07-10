// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_data::{LogsData, Severity};
use fuchsia_zircon_types::zx_koid_t;
use moniker::AbsoluteMoniker;
use std::str::FromStr;

use crate::{
    log_formatter::{LogData, LogEntry, LogSpamFilter},
    LogCommand,
};

/// A struct that holds the criteria for filtering logs.
pub struct LogFilterCriteria {
    /// The minimum severity of logs to include.
    min_severity: Severity,
    /// Filter by string.
    filters: Vec<String>,
    /// Monikers to include in logs.
    moniker_filters: Vec<String>,
    /// Exclude by string.
    excludes: Vec<String>,
    /// The tags to include.
    tags: Vec<String>,
    /// The tags to exclude.
    exclude_tags: Vec<String>,
    /// Spam filter
    spam_filter: Option<Box<dyn LogSpamFilter>>,
    /// Filter by PID
    pid: Option<zx_koid_t>,
    /// Filter by TID
    tid: Option<zx_koid_t>,
}

impl Default for LogFilterCriteria {
    fn default() -> Self {
        Self {
            min_severity: Severity::Info,
            filters: vec![],
            excludes: vec![],
            tags: vec![],
            moniker_filters: vec![],
            exclude_tags: vec![],
            pid: None,
            tid: None,
            spam_filter: None,
        }
    }
}

impl TryFrom<&LogCommand> for LogFilterCriteria {
    type Error = anyhow::Error;

    fn try_from(cmd: &LogCommand) -> Result<Self, Self::Error> {
        Ok(Self {
            min_severity: cmd.severity,
            filters: cmd.filter.clone(),
            tags: cmd.tags.clone(),
            excludes: cmd.exclude.clone(),
            moniker_filters: cmd.moniker.clone(),
            exclude_tags: cmd.exclude_tags.clone(),
            pid: cmd.pid.clone(),
            tid: cmd.tid.clone(),
            spam_filter: None,
        })
    }
}

impl LogFilterCriteria {
    /// Sets the minimum severity of logs to include.
    pub fn set_min_severity(&mut self, severity: Severity) {
        self.min_severity = severity;
    }

    /// Sets the tags to include.
    pub fn set_tags<I, S>(&mut self, tags: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tags = tags.into_iter().map(|value| value.into()).collect();
    }

    /// Sets the tags to exclude.
    pub fn set_exclude_tags<I, S>(&mut self, tags: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.exclude_tags = tags.into_iter().map(|value| value.into()).collect();
    }

    /// Returns true if the given `LogEntry` matches the filter criteria.
    pub fn matches(&self, entry: &LogEntry) -> bool {
        match entry {
            LogEntry { data: LogData::TargetLog(data), .. } => self.match_filters_to_log_data(data),
            LogEntry { data: LogData::SymbolizedTargetLog(data, _), .. } => {
                self.match_filters_to_log_data(data)
            }
            LogEntry { data: LogData::FfxEvent(_), .. } => true,
            LogEntry { data: LogData::MalformedTargetLog(_), .. } => true,
        }
    }

    /// Returns true if this is spam.
    pub fn is_spam(&self, entry: &LogEntry) -> bool {
        match entry {
            LogEntry { data: LogData::TargetLog(data), .. } => self.inner_is_spam(
                data.metadata.file.as_ref().map(|value| value.as_str()),
                data.metadata.line,
                data.msg().unwrap_or(""),
            ),
            LogEntry { data: LogData::SymbolizedTargetLog(data, message), .. } => self
                .inner_is_spam(
                    data.metadata.file.as_ref().map(|value| value.as_str()),
                    data.metadata.line,
                    message.as_str(),
                ),
            _ => false,
        }
    }

    /// Sets a spam filter
    pub fn with_spam_filter<S>(&mut self, spam_filter: S)
    where
        S: LogSpamFilter + 'static,
    {
        self.spam_filter = Some(Box::new(spam_filter));
    }

    /// Returns true if the given `LogsData` matches the filter criteria.
    fn inner_is_spam(&self, file: Option<&str>, line: Option<u64>, msg: &str) -> bool {
        match &self.spam_filter {
            None => false,
            Some(f) => f.is_spam(file, line, msg),
        }
    }

    /// Returns true if the given 'LogsData' matches the filter string by
    /// message, moniker, or component URL.
    fn matches_filter_string(filter_string: &str, message: &str, log: &LogsData) -> bool {
        let filter_moniker = AbsoluteMoniker::from_str(filter_string);
        let moniker = AbsoluteMoniker::from_str(log.moniker.as_str());
        return message.contains(filter_string)
            || log.moniker.contains(filter_string)
            || moniker.clone().map(|m| m.to_string().contains(filter_string)).unwrap_or(false)
            || match (moniker, filter_moniker) {
                (Ok(m), Ok(f)) => m.to_string().contains(&f.to_string()),
                _ => false,
            }
            || log.metadata.component_url.as_ref().map_or(false, |s| s.contains(filter_string));
    }

    /// Returns true if the given `LogsData` matches the moniker string.
    fn matches_filter_by_moniker_string(filter_string: &str, log: &LogsData) -> bool {
        let filter_moniker = AbsoluteMoniker::from_str(filter_string);
        let moniker = AbsoluteMoniker::from_str(log.moniker.as_str());
        matches!((moniker, filter_moniker), (Ok(a), Ok(b)) if a == b)
    }

    /// Returns true if the given `LogsData` matches the filter criteria.
    fn match_filters_to_log_data(&self, data: &LogsData) -> bool {
        if data.metadata.severity < self.min_severity {
            return false;
        }

        if let Some(pid) = self.pid {
            if data.pid() != Some(pid) {
                return false;
            }
        }

        if let Some(tid) = self.tid {
            if data.tid() != Some(tid) {
                return false;
            }
        }

        if !self.moniker_filters.is_empty()
            && !self
                .moniker_filters
                .iter()
                .any(|f| Self::matches_filter_by_moniker_string(f, &data))
        {
            return false;
        }

        let msg = data.msg().unwrap_or("");

        if !self.filters.is_empty()
            && !self.filters.iter().any(|f| Self::matches_filter_string(f, msg, &data))
        {
            return false;
        }

        if self.excludes.iter().any(|f| Self::matches_filter_string(f, msg, &data)) {
            return false;
        }

        if !self.tags.is_empty()
            && !self.tags.iter().any(|f| data.tags().map(|t| t.contains(f)).unwrap_or(false))
        {
            return false;
        }

        if self.exclude_tags.iter().any(|f| data.tags().map(|t| t.contains(f)).unwrap_or(false)) {
            return false;
        }

        true
    }
}

#[cfg(test)]
mod test {
    use assert_matches::assert_matches;
    use diagnostics_data::Timestamp;
    use std::time::Duration;

    use crate::{log_formatter::EventType, DumpCommand, LogSubCommand, SessionSpec};

    use super::*;

    const DEFAULT_TS_NANOS: u64 = 1615535969000000000;

    fn empty_dump_command() -> LogCommand {
        LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {
                session: SessionSpec::Relative(0),
            })),
            ..LogCommand::default()
        }
    }

    fn default_ts() -> Duration {
        Duration::from_nanos(DEFAULT_TS_NANOS)
    }

    fn make_log_entry(log_data: LogData) -> LogEntry {
        LogEntry { timestamp: Timestamp::from(default_ts().as_nanos() as i64), data: log_data }
    }

    #[fuchsia::test]
    async fn test_criteria_tag_filter() {
        let cmd = LogCommand {
            tags: vec!["tag1".to_string()],
            exclude_tags: vec!["tag3".to_string()],
            ..empty_dump_command()
        };
        let criteria = LogFilterCriteria::try_from(&cmd).unwrap();

        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: String::default(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included")
            .add_tag("tag1")
            .add_tag("tag2")
            .build()
            .into()
        )));

        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: String::default(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included")
            .add_tag("tag2")
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: String::default(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included")
            .add_tag("tag1")
            .add_tag("tag3")
            .build()
            .into()
        )));
    }

    #[fuchsia::test]
    async fn test_severity_filter_with_debug() {
        let mut cmd = empty_dump_command();
        cmd.severity = Severity::Trace;
        let criteria = LogFilterCriteria::try_from(&cmd).unwrap();

        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "included/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "included/moniker".to_string(),
                severity: diagnostics_data::Severity::Info,
            })
            .set_message("different message")
            .build()
            .into()
        )));
        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "other/moniker".to_string(),
                severity: diagnostics_data::Severity::Debug,
            })
            .set_message("included message")
            .build()
            .into()
        )));
    }

    #[fuchsia::test]
    async fn test_pid_filter() {
        let mut cmd = empty_dump_command();
        cmd.pid = Some(123);
        let criteria = LogFilterCriteria::try_from(&cmd).unwrap();

        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "included/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .set_pid(123)
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "included/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .set_pid(456)
            .build()
            .into()
        )));
    }

    #[fuchsia::test]
    async fn test_criteria_moniker_filter() {
        let cmd = LogCommand {
            moniker: vec!["/core/network/netstack".to_string()],
            ..empty_dump_command()
        };
        let criteria = LogFilterCriteria::try_from(&cmd).unwrap();

        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "bootstrap/archivist".into(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("excluded")
            .build()
            .into()
        )));

        let allowed_cmd = LogCommand { ..empty_dump_command() };
        let allowed_criteria = LogFilterCriteria::try_from(&allowed_cmd).unwrap();
        assert!(allowed_criteria
            .matches(&make_log_entry(LogData::FfxEvent(EventType::LoggingStarted),)));

        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "core/network/netstack".into(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included")
            .build()
            .into()
        )));

        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "core/network/dhcp".into(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included")
            .build()
            .into()
        )));
    }

    #[fuchsia::test]
    async fn test_tid_filter() {
        let mut cmd = empty_dump_command();
        cmd.tid = Some(123);
        let criteria = LogFilterCriteria::try_from(&cmd).unwrap();

        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "included/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .set_tid(123)
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "included/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .set_tid(456)
            .build()
            .into()
        )));
    }

    #[fuchsia::test]
    async fn test_setter_functions() {
        let mut filter = LogFilterCriteria::default();
        filter.set_min_severity(Severity::Error);
        assert_eq!(filter.min_severity, Severity::Error);
        filter.set_tags(["tag1"]);
        assert_eq!(filter.tags, ["tag1"]);
        filter.set_exclude_tags(["tag2"]);
        assert_eq!(filter.exclude_tags, ["tag2"]);
    }

    #[fuchsia::test]
    async fn test_criteria_moniker_message_and_severity_matches() {
        let cmd = LogCommand {
            filter: vec!["included".to_string()],
            exclude: vec!["not this".to_string()],
            severity: Severity::Error,
            ..empty_dump_command()
        };
        let criteria = LogFilterCriteria::try_from(&cmd).unwrap();

        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "included/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "included/moniker".to_string(),
                severity: diagnostics_data::Severity::Fatal,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                // Include a "/" prefix on the moniker to test filter permissiveness.
                moniker: "/included/moniker".to_string(),
                severity: diagnostics_data::Severity::Fatal,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "not/this/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("different message")
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "included/moniker".to_string(),
                severity: diagnostics_data::Severity::Warn,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "other/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("not this message")
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "included/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("not this message")
            .build()
            .into()
        )));
    }

    #[fuchsia::test]
    async fn test_empty_criteria() {
        let cmd = empty_dump_command();
        let criteria = LogFilterCriteria::try_from(&cmd).unwrap();

        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "included/moniker".to_string(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "included/moniker".to_string(),
                severity: diagnostics_data::Severity::Info,
            })
            .set_message("different message")
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "other/moniker".to_string(),
                severity: diagnostics_data::Severity::Debug,
            })
            .set_message("included message")
            .build()
            .into()
        )));

        let mut entry = make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp_nanos: 0.into(),
                component_url: Some(String::default()),
                moniker: "other/moniker".to_string(),
                severity: diagnostics_data::Severity::Debug,
            })
            .set_message("included message")
            .build()
            .into(),
        );
        entry.data = assert_matches!(
            entry.data.clone(),
            LogData::TargetLog(d) => LogData::SymbolizedTargetLog(d, "symbolized".to_string())
        );

        assert!(!criteria.matches(&entry));

        assert!(criteria.matches(&make_log_entry(LogData::FfxEvent(EventType::LoggingStarted))));

        let entry = make_log_entry(LogData::MalformedTargetLog("malformed".to_string()));
        assert!(criteria.matches(&entry));
    }
}
