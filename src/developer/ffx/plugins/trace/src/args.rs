// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use argh::FromArgs;
use ffx_core::ffx_command;
use fidl_fuchsia_developer_ffx::{Action, Trigger};
use fidl_fuchsia_tracing::BufferingMode;

#[ffx_command()]
#[derive(FromArgs, Debug, PartialEq)]
/// Interact with the tracing subsystem.
#[argh(subcommand, name = "trace")]
pub struct TraceCommand {
    #[argh(subcommand)]
    pub sub_cmd: TraceSubCommand,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
pub enum TraceSubCommand {
    ListCategories(ListCategories),
    ListProviders(ListProviders),
    ListCategoryGroups(ListCategoryGroups),
    Start(Start),
    Stop(Stop),
    Status(Status),
    // More commands including `record` and `convert` to follow.
}

#[derive(FromArgs, PartialEq, Debug)]
/// List the target's known trace categories.
#[argh(subcommand, name = "list-categories")]
pub struct ListCategories {}

#[derive(FromArgs, PartialEq, Debug)]
/// List the target's trace providers.
#[argh(subcommand, name = "list-providers")]
pub struct ListProviders {}

#[derive(FromArgs, PartialEq, Debug)]
/// List the builtin and custom category groups.
#[argh(subcommand, name = "list-category-groups")]
pub struct ListCategoryGroups {}

// Work around argh's handling of Vec.  Listing categories as a comma
// separated list of values rather than a repeated keyed option
// is much more concise when dealing with a large set of categories.
pub type TraceCategories = Vec<String>;

#[derive(FromArgs, PartialEq, Debug)]
/// Gets status of all running traces.
#[argh(subcommand, name = "status")]
pub struct Status {}

#[derive(FromArgs, PartialEq, Debug)]
/// Stops an active running trace.
#[argh(subcommand, name = "stop")]
pub struct Stop {
    /// name of output trace file. Relative or absolute paths are both supported.
    /// If this is not supplied (the default), then the default target will be
    /// used as the method to stop the trace.
    #[argh(option)]
    pub output: Option<String>,
}

#[derive(FromArgs, PartialEq, Debug)]
/// Record a trace.
#[argh(subcommand, name = "start")]
pub struct Start {
    /// the buffering to use on the trace. Defaults to "oneshot".
    /// Accepted values are "oneshot," "circular," and "streaming."
    #[argh(option, default = "BufferingMode::Oneshot", from_str_fn(buffering_mode_from_str))]
    pub buffering_mode: BufferingMode,

    /// size of per-provider trace buffer in MB.  Defaults to 4.
    #[argh(option, default = "4")]
    pub buffer_size: u32,

    /// comma-separated list of categories to enable.  Defaults
    /// to #default. Run `ffx config get trace.category_groups.default`
    /// to see what categories are included in #default.
    ///
    /// A trailing * may be used to indicate a prefix match. For example,
    /// kernel* would match any category that starts with kernel.
    ///
    /// A name prefixed with # indicates a category group that will be expanded
    /// from ffx config within the plugin *before* being sent to trace manager.
    /// A category group can either be added to global config by editing
    /// data/config.json in the ffx trace plugin, or by using ffx config set to
    /// add/edit a user configured category group. Available category groups
    /// can be discovered by running
    /// `ffx config get -s all trace.category_groups`
    #[argh(option, default = "vec![String::from(\"#default\")]", from_str_fn(parse_categories))]
    pub categories: TraceCategories,

    /// duration of trace capture in seconds.
    #[argh(option)]
    pub duration: Option<f64>,

    /// name of output trace file.  Defaults to trace.fxt.
    #[argh(option, default = "String::from(\"trace.fxt\")")]
    pub output: String,

    /// whether to run the trace in the background. Defaults to false,
    /// which means the trace will run in "interactive" mode.
    #[argh(switch)]
    pub background: bool,

    /// a trigger consists of an alert leading to an action. An alert
    /// is written into the code being traced, and an action here is
    /// what to do when the alert has happened. The list of supported
    /// actions are: 'terminate'.
    ///
    /// The expected format is "<alert>:<action>" ex:
    ///   -trigger "my_alert:terminate"
    ///
    /// This can be used with a duration, but keep in mind that this
    /// introduces a race between whether the alert leads to an
    /// action, and when the trace stops from the duration being
    /// reached.
    ///
    /// Triggers can only be used in the background outside of
    /// interactive mode.
    #[argh(option, from_str_fn(trigger_from_str))]
    pub trigger: Vec<Trigger>,
}

fn try_string_to_action(s: &str) -> Result<Action, String> {
    match s {
        "terminate" => Ok(Action::Terminate),
        e => Err(format!("unsupported action: \"{}\"", e)),
    }
}

fn trigger_from_str(val: &str) -> Result<Trigger, String> {
    let (alert, action) = val
        .split_once(":")
        .ok_or("triggers must be delimited by ':'. Example: '-trigger \"my_alert:terminate\"")?;
    let action = Some(try_string_to_action(action)?);
    let alert = Some(alert.to_owned());
    Ok(Trigger { alert, action, ..Trigger::EMPTY })
}

fn buffering_mode_from_str(val: &str) -> Result<BufferingMode, String> {
    match val {
        "o" | "oneshot" => Ok(BufferingMode::Oneshot),
        "c" | "circular" => Ok(BufferingMode::Circular),
        "s" | "streaming" => Ok(BufferingMode::Streaming),
        _ => Err(
            "Unrecognized value. Possible values are \"oneshot\", \"circular\", or \"streaming\""
                .to_owned(),
        ),
    }
}

fn parse_categories(value: &str) -> Result<TraceCategories, String> {
    let mut cats = Vec::new();

    if value.is_empty() {
        return Err("no categories specified".to_string());
    }

    for cat in value.split(",").map(|category| category.trim()) {
        if cat.is_empty() {
            return Err("empty category specified".to_string());
        }
        cats.push(String::from(cat));
    }

    Ok(cats)
}

#[cfg(test)]
mod tests {
    use super::*;
    const START_CMD_NAME: &'static [&'static str] = &["start"];

    #[test]
    fn test_parse_categories() {
        assert_eq!(parse_categories(&"a"), Ok(vec!["a".to_string()]));

        assert_eq!(
            parse_categories(&"a,b,c:d"),
            Ok(vec!["a".to_string(), "b".to_string(), "c:d".to_string()])
        );

        assert_eq!(parse_categories(&""), Err("no categories specified".to_string()));
        assert_eq!(parse_categories(&"a,,b"), Err("empty category specified".to_string()));
    }

    #[test]
    fn test_default_categories() {
        // This tests the code in a string that is passed as a default to argh. It is compile
        // checked because of the generated code, but this ensures that it is functionally correct.
        let cmd = Start::from_args(START_CMD_NAME, &[]).unwrap();
        assert_eq!(vec!["#default"], cmd.categories);
    }

    #[test]
    fn test_buffering_mode_from_str() {
        assert_eq!(buffering_mode_from_str("o"), Ok(BufferingMode::Oneshot));
        assert_eq!(buffering_mode_from_str("oneshot"), Ok(BufferingMode::Oneshot));
        assert_eq!(buffering_mode_from_str("c"), Ok(BufferingMode::Circular));
        assert_eq!(buffering_mode_from_str("circular"), Ok(BufferingMode::Circular));
        assert_eq!(buffering_mode_from_str("s"), Ok(BufferingMode::Streaming));
        assert_eq!(buffering_mode_from_str("streaming"), Ok(BufferingMode::Streaming));
    }

    #[test]
    fn test_unsupported_buffering_mode_from_str() {
        assert!(buffering_mode_from_str("roundrobin").is_err());
    }

    #[test]
    fn test_trigger_from_str_no_colon() {
        assert!(trigger_from_str("foobar").is_err());
    }

    #[test]
    fn test_trigger_from_str_invalid_action() {
        assert!(trigger_from_str("foobar:storp").is_err());
    }

    #[test]
    fn test_trigger_from_str_valid() {
        assert_eq!(
            trigger_from_str("foobar:terminate").unwrap(),
            Trigger {
                alert: Some("foobar".to_owned()),
                action: Some(Action::Terminate),
                ..Trigger::EMPTY
            },
        );
    }
}
