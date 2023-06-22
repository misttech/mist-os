// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_data::{LogsData, Severity};

use crate::{
    log_formatter::{LogData, LogEntry},
    LogCommand,
};

/// A struct that holds the criteria for filtering logs.
pub struct LogFilterCriteria {
    /// The minimum severity of logs to include.
    min_severity: Severity,
    /// The tags to include.
    tags: Vec<String>,
    /// The tags to exclude.
    exclude_tags: Vec<String>,
}

impl Default for LogFilterCriteria {
    fn default() -> Self {
        Self { min_severity: Severity::Info, tags: vec![], exclude_tags: vec![] }
    }
}

impl TryFrom<&LogCommand> for LogFilterCriteria {
    type Error = anyhow::Error;

    fn try_from(cmd: &LogCommand) -> Result<Self, Self::Error> {
        Ok(Self {
            min_severity: cmd.severity,
            tags: cmd.tags.clone(),
            exclude_tags: cmd.exclude_tags.clone(),
        })
    }
}

impl LogFilterCriteria {
    /// Returns true if the given `LogsData` matches the filter criteria.
    fn match_filters_to_log_data(&self, data: &LogsData) -> bool {
        if data.metadata.severity < self.min_severity {
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
