// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    ffx_target_show_args::TargetShow,
    schemars::JsonSchema,
    serde::Serialize,
    std::default::Default,
    std::io::Write,
    //TODO(https://fxbug.dev/42151881): Consider switching to crossterm.
    termion::{color, style},
};

/// Store, organize, and display hierarchical show information. Output may be
/// formatted for a human reader or structured as JSON for machine consumption.

const INDENT: usize = 4;

/// Show entry values.
#[derive(Debug, JsonSchema, PartialEq, Serialize)]
#[serde(untagged)]
pub enum ShowValue {
    BoolValue(bool),
    StringValue(String),
    StringListValue(Vec<String>),
}

impl std::fmt::Display for ShowValue {
    fn fmt<'a>(&self, f: &mut std::fmt::Formatter<'a>) -> std::fmt::Result {
        match self {
            ShowValue::BoolValue(value) => write!(f, "{:?}", value),
            ShowValue::StringValue(value) => write!(f, "{:?}", value),
            ShowValue::StringListValue(value) => write!(f, "{:?}", value),
        }
    }
}

/// A node in a hierarchy of show information or groupings.
#[derive(Default, Debug, JsonSchema, PartialEq, Serialize)]
pub struct ShowEntry {
    pub title: String,
    pub label: String,
    pub description: String,
    pub value: Option<ShowValue>,
    pub child: Vec<ShowEntry>,
    #[serde(skip_serializing)]
    #[schemars(skip)]
    pub highlight: bool,
}

impl ShowEntry {
    pub fn new(title: &str, label: &str, description: &str) -> Self {
        Self {
            title: title.to_string(),
            label: label.to_string(),
            description: description.to_string(),
            ..Default::default()
        }
    }

    // Create a Group ShowEntry.
    pub fn group(human_name: &str, id_name: &str, desc: &str, value: Vec<ShowEntry>) -> ShowEntry {
        let mut entry = ShowEntry::new(human_name, id_name, desc);
        entry.child = value;
        entry
    }

    // Create a Boolean ShowEntry.
    pub fn bool_value(
        human_name: &str,
        id_name: &str,
        desc: &str,
        value: &Option<bool>,
    ) -> ShowEntry {
        let mut entry = ShowEntry::new(human_name, id_name, desc);
        entry.value = value.map(|v| ShowValue::BoolValue(v));
        entry
    }

    // Create a string ShowEntry.
    pub fn str_value(
        human_name: &str,
        id_name: &str,
        desc: &str,
        value: &Option<String>,
    ) -> ShowEntry {
        let mut entry = ShowEntry::new(human_name, id_name, desc);
        entry.value = value.as_ref().map(|v| ShowValue::StringValue(v.to_string()));
        entry
    }

    // Create a string ShowEntry with color.
    pub fn str_value_with_highlight(
        human_name: &str,
        id_name: &str,
        desc: &str,
        value: &Option<String>,
    ) -> ShowEntry {
        let mut entry = ShowEntry::new(human_name, id_name, desc);
        entry.value = value.as_ref().map(|v| ShowValue::StringValue(v.to_string()));
        entry.highlight = true;
        entry
    }
}

/// Write output for easy reading by humans.
fn output_list<W: Write>(
    indent: usize,
    showes: &Vec<ShowEntry>,
    args: &TargetShow,
    writer: &mut W,
) -> Result<()> {
    for show in showes {
        let mut label = "".to_string();
        if args.label && !show.label.is_empty() {
            label = format!(" ({})", show.label);
        }
        let mut desc = "".to_string();
        if args.desc && !show.description.is_empty() {
            desc = format!(" # {}", show.description);
        }
        match &show.value {
            Some(value) => {
                if show.highlight {
                    writeln!(
                        *writer,
                        "{: <7$}{}{}: {}{}{}{}",
                        "",
                        show.title,
                        label,
                        color::Fg(color::Green),
                        value,
                        style::Reset,
                        desc,
                        indent
                    )?;
                } else {
                    writeln!(
                        *writer,
                        "{: <5$}{}{}: {}{}",
                        "", show.title, label, value, desc, indent
                    )?;
                }
            }
            None => writeln!(*writer, "{: <4$}{}{}: {}", "", show.title, label, desc, indent)?,
        }
        output_list(indent + INDENT, &show.child, args, writer)?;
    }
    Ok(())
}

/// Write output in English for easy reading by users.
pub fn output_for_human<W: Write>(
    showes: &Vec<ShowEntry>,
    args: &TargetShow,
    writer: &mut W,
) -> Result<()> {
    output_list(0, &showes, args, writer)
}

/// Write output in JSON for easy parsing by other tools.
pub fn output_for_machine<W: Write>(
    showes: &Vec<ShowEntry>,
    _args: &TargetShow,
    writer: &mut W,
) -> Result<()> {
    Ok(write!(writer, "{}", serde_json::to_string(&showes)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_display_output() {
        assert_eq!(format!("{}", ShowValue::BoolValue(false)), "false");
        assert_eq!(format!("{}", ShowValue::BoolValue(true)), "true");
        assert_eq!(format!("{}", ShowValue::StringValue("abc".to_string())), "\"abc\"");
        assert_eq!(format!("{}", ShowValue::StringValue("ab\"c".to_string())), "\"ab\\\"c\"");
        assert_eq!(
            format!("{}", ShowValue::StringListValue(vec!["abc".to_string(), "def".to_string()])),
            "[\"abc\", \"def\"]"
        );
    }

    #[test]
    fn test_output_list() {
        let input = vec![ShowEntry::new("Test", "the_test", "A test.")];
        let mut result = Vec::new();
        output_list(7, &input, &TargetShow::default(), &mut result).unwrap();
        assert_eq!(result, b"       Test: \n");
    }

    #[test]
    fn test_output_list_with_child() {
        let input = vec![ShowEntry::group(
            "Test",
            "the_test",
            "A test.",
            vec![ShowEntry::new("Prop", "a_prop", "Some data.")],
        )];
        let mut result = Vec::new();
        output_list(0, &input, &TargetShow::default(), &mut result).unwrap();
        assert_eq!(result, b"Test: \n    Prop: \n");
    }

    #[test]
    fn test_output_for_human() {
        let input = vec![ShowEntry::group(
            "Test",
            "the_test",
            "A test.",
            vec![ShowEntry::bool_value("Prop", "a_prop", "Some data.", &Some(false))],
        )];
        let mut result = Vec::new();
        output_for_human(&input, &TargetShow::default(), &mut result).unwrap();
        assert_eq!(result, b"Test: \n    Prop: false\n");
    }

    #[test]
    fn test_output_for_machine() {
        let input = vec![ShowEntry::group(
            "Test",
            "the_test",
            "A test.",
            vec![ShowEntry::bool_value("Prop", "a_prop", "Some data.", &Some(false))],
        )];
        let mut result = Vec::new();
        output_for_machine(&input, &TargetShow { json: true, ..Default::default() }, &mut result)
            .unwrap();
        let actual = String::from_utf8(result).expect(" machine bytes should be valid utf8");

        assert_eq!(
            actual,
            "[{\"title\":\"Test\",\"label\":\"the_test\",\"description\"\
                             :\"A test.\",\"value\":null,\"child\":[{\"title\":\"Prop\",\"label\":\"a_prop\",\
                             \"description\":\"Some data.\",\"value\":false,\"child\":[]}]}]"
        );
    }
}
