// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::log_formatter::{LogData, LogEntry};
use crate::{InstanceGetter, LogCommand, LogError};
use diagnostics_data::{LogsData, Severity};
use fidl_fuchsia_diagnostics::LogInterestSelector;
use moniker::{ExtendedMoniker, EXTENDED_MONIKER_COMPONENT_MANAGER_STR};
use selectors::SelectorExt;
use std::borrow::Cow;
use std::str::FromStr;
use std::sync::LazyLock;
use zx_types::zx_koid_t;

static KLOG: &str = "klog";
static KLOG_MONIKER: LazyLock<ExtendedMoniker> =
    LazyLock::new(|| ExtendedMoniker::try_from(KLOG).unwrap());

struct MonikerFilters {
    queries: Vec<String>,
    matched_monikers: Vec<String>,
}

impl MonikerFilters {
    fn new(queries: Vec<String>) -> Self {
        Self { queries, matched_monikers: vec![] }
    }

    async fn expand_monikers(&mut self, getter: &impl InstanceGetter) -> Result<(), LogError> {
        self.matched_monikers = vec![];
        self.matched_monikers.reserve(self.queries.len());
        for query in &self.queries {
            if query == KLOG {
                self.matched_monikers.push(query.clone());
                continue;
            }

            let mut instances = getter.get_monikers_from_query(query).await?;
            if instances.len() > 1 {
                return Err(LogError::too_many_fuzzy_matches(
                    instances.into_iter().map(|i| i.to_string()),
                ));
            }
            match instances.pop() {
                Some(instance) => self.matched_monikers.push(instance.to_string()),
                None => return Err(LogError::SearchParameterNotFound(query.to_string())),
            }
        }

        Ok(())
    }
}

/// A struct that holds the criteria for filtering logs.
pub struct LogFilterCriteria {
    /// The minimum severity of logs to include.
    min_severity: Severity,
    /// Filter by string.
    filters: Vec<String>,
    /// Monikers to include in logs.
    moniker_filters: MonikerFilters,
    /// Exclude by string.
    excludes: Vec<String>,
    /// The tags to include.
    tags: Vec<String>,
    /// The tags to exclude.
    exclude_tags: Vec<String>,
    /// Filter by PID
    pid: Option<zx_koid_t>,
    /// Filter by TID
    tid: Option<zx_koid_t>,
    /// Log interest selectors used to filter severity on a per-component basis
    /// Overrides min_severity for components matching the selector.
    /// In the event of an ambiguous match, the lowest severity is used.
    interest_selectors: Vec<LogInterestSelector>,
    /// True if case sensitive, false otherwise
    case_sensitive: bool,
}

impl Default for LogFilterCriteria {
    fn default() -> Self {
        Self {
            min_severity: Severity::Info,
            filters: vec![],
            excludes: vec![],
            tags: vec![],
            moniker_filters: MonikerFilters::new(vec![]),
            exclude_tags: vec![],
            pid: None,
            tid: None,
            case_sensitive: false,
            interest_selectors: vec![],
        }
    }
}

// Convert a string to lowercase if needed for case insensitive comparisons.
// If case_sensitive is false, the conversion is performed.
fn convert_to_lowercase_if_needed<'a>(input: &'a str, case_sensitive: bool) -> Cow<'a, str> {
    if case_sensitive {
        Cow::Borrowed(input)
    } else {
        Cow::Owned(input.to_lowercase())
    }
}

impl From<LogCommand> for LogFilterCriteria {
    fn from(mut cmd: LogCommand) -> Self {
        Self {
            min_severity: cmd.severity,
            filters: cmd.filter,
            tags: cmd
                .tag
                .into_iter()
                .map(|value| convert_to_lowercase_if_needed(&value, cmd.case_sensitive).to_string())
                .collect(),
            excludes: cmd.exclude,
            moniker_filters: if cmd.kernel {
                cmd.component.push(KLOG.to_string());
                MonikerFilters::new(cmd.component)
            } else {
                MonikerFilters::new(cmd.component)
            },
            exclude_tags: cmd.exclude_tags,
            pid: cmd.pid,
            case_sensitive: cmd.case_sensitive,
            tid: cmd.tid,
            interest_selectors: cmd.set_severity.into_iter().flatten().collect(),
        }
    }
}

impl LogFilterCriteria {
    /// Sets the minimum severity of logs to include.
    pub fn set_min_severity(&mut self, severity: Severity) {
        self.min_severity = severity;
    }

    pub async fn expand_monikers(&mut self, getter: &impl InstanceGetter) -> Result<(), LogError> {
        self.moniker_filters.expand_monikers(getter).await
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
        }
    }

    /// Returns true if the given 'LogsData' matches the filter string by
    /// message, moniker, or component URL.
    fn matches_filter_string(
        filter_string: &str,
        message: &str,
        log: &LogsData,
        case_sensitive: bool,
    ) -> bool {
        // Convert strings to lower-case if needed
        let filter_string = convert_to_lowercase_if_needed(filter_string, case_sensitive);
        let message = convert_to_lowercase_if_needed(message, case_sensitive);
        let file_path =
            log.file_path().map(|value| convert_to_lowercase_if_needed(value, case_sensitive));
        let component_url = log
            .metadata
            .component_url
            .as_ref()
            .map(|value| convert_to_lowercase_if_needed(value.as_str(), case_sensitive));
        let moniker_str = log.moniker.to_string();
        let moniker = convert_to_lowercase_if_needed(&moniker_str, case_sensitive);

        message.contains(&*filter_string)
            || file_path.is_some_and(|s| s.contains(&*filter_string))
            || component_url.as_ref().is_some_and(|s| s.contains(&*filter_string))
            || moniker.contains(&*filter_string)
    }

    // TODO(b/303315896): If/when debuglog is structured remove this.
    fn parse_tags(value: &str) -> Vec<&str> {
        let mut tags = Vec::new();
        let mut current = value;
        if !current.starts_with('[') {
            return tags;
        }
        loop {
            match current.find('[') {
                Some(opening_index) => {
                    current = &current[opening_index + 1..];
                }
                None => return tags,
            }
            match current.find(']') {
                Some(closing_index) => {
                    tags.push(&current[..closing_index]);
                    current = &current[closing_index + 1..];
                }
                None => return tags,
            }
        }
    }

    fn match_synthetic_klog_tags(&self, klog_str: &str, case_sensitive: bool) -> bool {
        let tags = Self::parse_tags(klog_str)
            .into_iter()
            .map(|value| convert_to_lowercase_if_needed(value, case_sensitive))
            .collect::<Vec<_>>();
        self.tags.iter().any(|f| {
            tags.iter().any(|t| convert_to_lowercase_if_needed(t, case_sensitive).contains(f))
        })
    }

    /// Returns true if the given `LogsData` matches the moniker string.
    fn matches_filter_by_moniker_string(filter_string: &str, log: &LogsData) -> bool {
        let Ok(filter_moniker) = ExtendedMoniker::from_str(filter_string) else {
            return false;
        };
        filter_moniker == log.moniker
    }

    /// Returns true if the given `LogsData` matches the filter criteria.
    fn match_filters_to_log_data(&self, data: &LogsData) -> bool {
        let min_severity = self
            .interest_selectors
            .iter()
            .filter(|s| data.moniker.matches_component_selector(&s.selector).unwrap_or(false))
            .filter_map(|selector| selector.interest.min_severity)
            .min()
            .unwrap_or_else(|| self.min_severity.into());
        if data.metadata.severity < min_severity {
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

        if !self.moniker_filters.matched_monikers.is_empty()
            && !self
                .moniker_filters
                .matched_monikers
                .iter()
                .any(|f| Self::matches_filter_by_moniker_string(f, data))
        {
            return false;
        }

        let msg = data.msg().unwrap_or("");

        if !self.filters.is_empty()
            && !self
                .filters
                .iter()
                .any(|f| Self::matches_filter_string(f, msg, data, self.case_sensitive))
        {
            return false;
        }

        if self
            .excludes
            .iter()
            .any(|f| Self::matches_filter_string(f, msg, data, self.case_sensitive))
        {
            return false;
        }
        if !self.tags.is_empty()
            && !self.tags.iter().any(|query_tag| {
                let has_tag = data
                    .tags()
                    .map(|t| {
                        t.iter().any(|value| {
                            convert_to_lowercase_if_needed(value, self.case_sensitive) == *query_tag
                        })
                    })
                    .unwrap_or(false);
                let moniker_has_tag =
                    moniker_contains_in_last_segment(&data.moniker, query_tag, self.case_sensitive);
                has_tag || moniker_has_tag
            })
        {
            if data.moniker == *KLOG_MONIKER {
                return self
                    .match_synthetic_klog_tags(data.msg().unwrap_or(""), self.case_sensitive);
            }
            return false;
        }

        if self.exclude_tags.iter().any(|excluded_tag| {
            let has_tag = data.tags().map(|tag| tag.contains(excluded_tag)).unwrap_or(false);
            let moniker_has_tag =
                moniker_contains_in_last_segment(&data.moniker, excluded_tag, self.case_sensitive);
            has_tag || moniker_has_tag
        }) {
            return false;
        }

        true
    }
}

fn moniker_contains_in_last_segment(
    moniker: &ExtendedMoniker,
    query_tag: &str,
    case_sensitive: bool,
) -> bool {
    let query_tag = convert_to_lowercase_if_needed(query_tag, case_sensitive);
    match moniker {
        ExtendedMoniker::ComponentInstance(moniker) => moniker
            .path()
            .last()
            .map(|segment| {
                convert_to_lowercase_if_needed(&segment.to_string(), case_sensitive)
                    .contains(&*query_tag)
            })
            .unwrap_or(false),
        ExtendedMoniker::ComponentManager => {
            EXTENDED_MONIKER_COMPONENT_MANAGER_STR.contains(&*query_tag)
        }
    }
}

#[cfg(test)]
mod test {
    use diagnostics_data::{ExtendedMoniker, Timestamp};
    use selectors::parse_log_interest_selector;

    use crate::log_socket_stream::OneOrMany;
    use crate::{DumpCommand, LogSubCommand};

    use super::*;

    fn empty_dump_command() -> LogCommand {
        LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            ..LogCommand::default()
        }
    }

    fn make_log_entry(log_data: LogData) -> LogEntry {
        LogEntry { data: log_data }
    }

    #[fuchsia::test]
    async fn test_criteria_tag_filter_filters_moniker() {
        let cmd = LogCommand { tag: vec!["testcomponent".to_string()], ..empty_dump_command() };
        let criteria = LogFilterCriteria::from(cmd);

        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "my/testcomponent".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included")
            .add_tag("tag1")
            .add_tag("tag2")
            .build()
            .into()
        )));
    }

    #[fuchsia::test]
    async fn test_criteria_exclude_tag_filters_moniker() {
        let cmd =
            LogCommand { exclude_tags: vec!["testcomponent".to_string()], ..empty_dump_command() };
        let criteria = LogFilterCriteria::from(cmd);
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "my/testcomponent".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("excluded")
            .add_tag("tag1")
            .add_tag("tag2")
            .build()
            .into()
        )));
    }

    #[fuchsia::test]
    async fn test_criteria_tag_filter() {
        let cmd = LogCommand {
            tag: vec!["tag1".to_string()],
            exclude_tags: vec!["tag3".to_string()],
            ..empty_dump_command()
        };
        let criteria = LogFilterCriteria::from(cmd);

        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: ExtendedMoniker::ComponentManager,
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
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: ExtendedMoniker::ComponentManager,
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included")
            .add_tag("tag2")
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: ExtendedMoniker::ComponentManager,
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
    async fn test_per_component_severity() {
        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            set_severity: vec![OneOrMany::One(
                parse_log_interest_selector("test_selector#DEBUG").unwrap(),
            )],
            ..LogCommand::default()
        };
        let expectations = [
            ("test_selector", diagnostics_data::Severity::Debug, true),
            ("other_selector", diagnostics_data::Severity::Debug, false),
            ("other_selector", diagnostics_data::Severity::Info, true),
        ];
        let criteria = LogFilterCriteria::from(cmd);
        assert_eq!(criteria.min_severity, Severity::Info);
        for (moniker, severity, is_included) in expectations {
            let entry = make_log_entry(
                diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                    timestamp: Timestamp::from_nanos(0),
                    component_url: Some("".into()),
                    moniker: moniker.try_into().unwrap(),
                    severity,
                })
                .set_message("message")
                .add_tag("tag1")
                .add_tag("tag2")
                .build()
                .into(),
            );
            assert_eq!(criteria.matches(&entry), is_included);
        }
    }

    #[fuchsia::test]
    async fn test_per_component_severity_uses_min_match() {
        let severities = [
            diagnostics_data::Severity::Info,
            diagnostics_data::Severity::Trace,
            diagnostics_data::Severity::Debug,
        ];

        let cmd = LogCommand {
            sub_command: Some(LogSubCommand::Dump(DumpCommand {})),
            set_severity: vec![
                OneOrMany::One(parse_log_interest_selector("test_selector#INFO").unwrap()),
                OneOrMany::One(parse_log_interest_selector("test_selector#TRACE").unwrap()),
                OneOrMany::One(parse_log_interest_selector("test_selector#DEBUG").unwrap()),
            ],
            ..LogCommand::default()
        };
        let criteria = LogFilterCriteria::from(cmd);

        for severity in severities {
            let entry = make_log_entry(
                diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                    timestamp: Timestamp::from_nanos(0),
                    component_url: Some("".into()),
                    moniker: "test_selector".try_into().unwrap(),
                    severity,
                })
                .set_message("message")
                .add_tag("tag1")
                .add_tag("tag2")
                .build()
                .into(),
            );
            assert!(criteria.matches(&entry));
        }
    }

    #[fuchsia::test]
    async fn test_criteria_tag_filter_legacy() {
        let cmd = LogCommand {
            tag: vec!["tag1".to_string()],
            exclude_tags: vec!["tag3".to_string()],
            ..empty_dump_command()
        };
        let criteria = LogFilterCriteria::from(cmd);

        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: ExtendedMoniker::ComponentManager,
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
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: ExtendedMoniker::ComponentManager,
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included")
            .add_tag("tag2")
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: ExtendedMoniker::ComponentManager,
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
        let criteria = LogFilterCriteria::from(cmd);

        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "included/moniker".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "included/moniker".try_into().unwrap(),
                severity: diagnostics_data::Severity::Info,
            })
            .set_message("different message")
            .build()
            .into()
        )));
        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "other/moniker".try_into().unwrap(),
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
        let criteria = LogFilterCriteria::from(cmd);

        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "included/moniker".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .set_pid(123)
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "included/moniker".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .set_pid(456)
            .build()
            .into()
        )));
    }

    struct FakeInstanceGetter;
    #[async_trait::async_trait(?Send)]
    impl InstanceGetter for FakeInstanceGetter {
        async fn get_monikers_from_query(
            &self,
            query: &str,
        ) -> Result<Vec<moniker::Moniker>, LogError> {
            Ok(vec![moniker::Moniker::try_from(query).unwrap()])
        }
    }

    #[fuchsia::test]
    async fn test_criteria_component_filter() {
        let cmd = LogCommand {
            component: vec!["/core/network/netstack".to_string()],
            ..empty_dump_command()
        };

        let mut criteria = LogFilterCriteria::from(cmd);
        criteria.expand_monikers(&FakeInstanceGetter).await.unwrap();

        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "bootstrap/archivist".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("excluded")
            .build()
            .into()
        )));

        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "core/network/netstack".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included")
            .build()
            .into()
        )));

        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "core/network/dhcp".try_into().unwrap(),
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
        let criteria = LogFilterCriteria::from(cmd);

        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "included/moniker".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .set_tid(123)
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "included/moniker".try_into().unwrap(),
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
        let criteria = LogFilterCriteria::from(cmd);

        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "included/moniker".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "included/moniker".try_into().unwrap(),
                severity: diagnostics_data::Severity::Fatal,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                // Include a "/" prefix on the moniker to test filter permissiveness.
                moniker: "included/moniker".try_into().unwrap(),
                severity: diagnostics_data::Severity::Fatal,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "not/this/moniker".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("different message")
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "included/moniker".try_into().unwrap(),
                severity: diagnostics_data::Severity::Warn,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "other/moniker".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("not this message")
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "included/moniker".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("not this message")
            .build()
            .into()
        )));
    }

    #[fuchsia::test]
    async fn test_criteria_klog_only() {
        let cmd = LogCommand { tag: vec!["component_manager".into()], ..empty_dump_command() };
        let criteria = LogFilterCriteria::from(cmd);

        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "klog".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("[component_manager] included message")
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "klog".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("excluded message[component_manager]")
            .build()
            .into()
        )));
        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "klog".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("[tag0][component_manager] included message")
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "klog".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("[other] excluded message")
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "klog".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("no tags, excluded")
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "other/moniker".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("[component_manager] excluded message")
            .build()
            .into()
        )));
    }

    #[fuchsia::test]
    async fn test_criteria_klog_tag_hack() {
        let cmd = LogCommand { kernel: true, ..empty_dump_command() };
        let mut criteria = LogFilterCriteria::from(cmd);

        criteria.expand_monikers(&FakeInstanceGetter).await.unwrap();

        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "klog".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "other/moniker".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .build()
            .into()
        )));
    }

    #[test]
    fn filter_fiters_filename() {
        let cmd = LogCommand { filter: vec!["sometestfile".into()], ..empty_dump_command() };
        let criteria = LogFilterCriteria::from(cmd);

        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "core/last_segment".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_file("sometestfile")
            .set_message("hello world")
            .build()
            .into()
        )));
    }

    #[fuchsia::test]
    async fn test_empty_criteria() {
        let cmd = empty_dump_command();
        let criteria = LogFilterCriteria::from(cmd);

        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "included/moniker".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("included message")
            .build()
            .into()
        )));
        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "included/moniker".try_into().unwrap(),
                severity: diagnostics_data::Severity::Info,
            })
            .set_message("different message")
            .build()
            .into()
        )));
        assert!(!criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "other/moniker".try_into().unwrap(),
                severity: diagnostics_data::Severity::Debug,
            })
            .set_message("included message")
            .build()
            .into()
        )));

        let entry = make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "other/moniker".try_into().unwrap(),
                severity: diagnostics_data::Severity::Debug,
            })
            .set_message("included message")
            .build()
            .into(),
        );

        assert!(!criteria.matches(&entry));
    }

    #[test]
    fn filter_fiters_case_sensitivity() {
        // Case-insensitive by default
        let cmd = LogCommand { filter: vec!["sometestfile".into()], ..empty_dump_command() };
        let criteria = LogFilterCriteria::from(cmd);

        let entry_0 = make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "core/last_segment".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_file("sometestfile")
            .set_message("hello world")
            .build()
            .into(),
        );

        let entry_1 = make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "core/last_segment".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_file("someTESTfile")
            .set_message("hello world")
            .build()
            .into(),
        );
        assert!(criteria.matches(&entry_0));
        assert!(criteria.matches(&entry_1));

        // Case-sensitive
        let cmd = LogCommand {
            filter: vec!["sometestfile".into()],
            case_sensitive: true,
            ..empty_dump_command()
        };
        let criteria = LogFilterCriteria::from(cmd);

        assert!(criteria.matches(&entry_0));
        assert!(!criteria.matches(&entry_1));
    }

    #[test]
    fn filter_fiters_case_sensitivity_for_tags() {
        // Case-insensitive by default
        let cmd = LogCommand { tag: vec!["someTAG".into()], ..empty_dump_command() };
        let criteria = LogFilterCriteria::from(cmd);

        let entry_0 = make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "core/last_segment".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .add_tag("someTAG")
            .set_message("hello world")
            .build()
            .into(),
        );

        let entry_1 = make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "core/last_segment".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .add_tag("SomeTaG")
            .set_message("hello world")
            .build()
            .into(),
        );
        assert!(criteria.matches(&entry_0));
        assert!(criteria.matches(&entry_1));

        // Case-sensitive
        let cmd = LogCommand {
            tag: vec!["someTAG".into()],
            case_sensitive: true,
            ..empty_dump_command()
        };
        let criteria = LogFilterCriteria::from(cmd);

        assert!(criteria.matches(&entry_0));
        assert!(!criteria.matches(&entry_1));
    }

    #[test]
    fn filter_fiters_case_sensitivity_for_tags_including_moniker() {
        // Case-insensitive by default
        let cmd = LogCommand { tag: vec!["someTAG".into()], ..empty_dump_command() };
        let criteria = LogFilterCriteria::from(cmd);

        let entry_0 = make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "core/someTAG".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("hello world")
            .build()
            .into(),
        );

        let entry_1 = make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "core/SomeTaG".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("hello world")
            .build()
            .into(),
        );
        assert!(criteria.matches(&entry_0));
        assert!(criteria.matches(&entry_1));

        // Case-sensitive
        let cmd = LogCommand {
            tag: vec!["someTAG".into()],
            case_sensitive: true,
            ..empty_dump_command()
        };
        let criteria = LogFilterCriteria::from(cmd);

        assert!(criteria.matches(&entry_0));
        assert!(!criteria.matches(&entry_1));
    }

    #[test]
    fn tag_matches_moniker_last_segment() {
        // When the tags are empty, the last segment of the moniker is treated as the tag.
        let cmd = LogCommand { tag: vec!["last_segment".to_string()], ..empty_dump_command() };
        let criteria = LogFilterCriteria::from(cmd);

        assert!(criteria.matches(&make_log_entry(
            diagnostics_data::LogsDataBuilder::new(diagnostics_data::BuilderArgs {
                timestamp: Timestamp::from_nanos(0),
                component_url: Some("".into()),
                moniker: "core/last_segment".try_into().unwrap(),
                severity: diagnostics_data::Severity::Error,
            })
            .set_message("hello world")
            .build()
            .into()
        )));
    }
}
