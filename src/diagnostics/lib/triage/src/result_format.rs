// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {crate::act::ActionResults, itertools::Itertools};

pub struct ActionResultFormatter<'a> {
    action_results: &'a ActionResults,
}

impl<'a> ActionResultFormatter<'a> {
    pub fn new(action_results: &ActionResults) -> ActionResultFormatter<'_> {
        ActionResultFormatter { action_results }
    }

    pub fn to_text(&self) -> String {
        match self.inner_to_text() {
            (true, v) => v,
            (false, v) if v.is_empty() => "No actions were triggered. All targets OK.".to_string(),
            (false, v) => {
                vec![v, "No actions were triggered. All targets OK.".to_string()].join("\n")
            }
        }
    }

    fn inner_to_text(&self) -> (bool, String) {
        let mut sections = vec![];
        let mut problem = false;
        if let Some(errors) = self.to_errors() {
            sections.push(errors);
            problem = true;
        }
        if let Some(gauges) = self.to_gauges() {
            sections.push(gauges);
        }
        if let Some(warnings) = self.to_warnings() {
            sections.push(warnings);
            problem = true;
        }
        if let Some((plugin_problem, plugins)) = self.to_plugins() {
            sections.push(plugins);
            problem = problem || plugin_problem
        }
        if self.action_results.verbose {
            if let Some(gauges) = self.to_broken_gauges() {
                sections.push(gauges);
            }
            if let Some(infos) = self.to_infos() {
                sections.push(infos);
            }
        }
        (problem, sections.join("\n"))
    }

    fn to_infos(&self) -> Option<String> {
        if self.action_results.infos.is_empty() {
            return None;
        }

        let header = Self::make_underline("Info");
        Some(format!(
            "{}{}\n",
            header,
            (&self.action_results.infos).into_iter().sorted().join("\n")
        ))
    }

    fn to_warnings(&self) -> Option<String> {
        if self.action_results.warnings.is_empty() {
            return None;
        }

        let header = Self::make_underline("Warnings");
        Some(format!(
            "{}{}\n",
            header,
            (&self.action_results.warnings).into_iter().sorted().join("\n")
        ))
    }

    fn to_errors(&self) -> Option<String> {
        if self.action_results.errors.is_empty() {
            return None;
        }

        let header = Self::make_underline("Errors");
        Some(format!(
            "{}{}\n",
            header,
            (&self.action_results.errors).into_iter().sorted().join("\n")
        ))
    }

    fn to_gauges(&self) -> Option<String> {
        if self.action_results.gauges.is_empty() {
            return None;
        }

        let header = Self::make_underline("Featured Values");
        let lines = &mut self.action_results.gauges.iter().cloned();
        let lines: Vec<String> = match self.action_results.sort_gauges {
            true => lines.sorted().collect(),
            false => lines.collect(),
        };

        Some(format!("{}{}\n", header, lines.join("\n")))
    }

    fn to_broken_gauges(&self) -> Option<String> {
        if self.action_results.broken_gauges.is_empty() {
            return None;
        }

        let header = Self::make_underline("Featured Values (Not Computable)");
        Some(format!(
            "{}{}\n",
            header,
            (&self.action_results.broken_gauges).into_iter().sorted().join("\n")
        ))
    }

    fn to_plugins(&self) -> Option<(bool, String)> {
        let mut warning = false;
        let results = self
            .action_results
            .sub_results
            .iter()
            .map(|(name, v)| {
                let fmt = ActionResultFormatter::new(&v);
                let val = match fmt.inner_to_text() {
                    (true, v) => {
                        warning = true;
                        format!("\n{}", v)
                    }
                    (false, v) if v.is_empty() => " - OK".to_string(),
                    (false, v) => format!(" - OK\n{}", v),
                };
                format!("{} Plugin{}", name, val)
            })
            .collect::<Vec<String>>();
        if results.is_empty() {
            None
        } else {
            Some((warning, results.join("\n")))
        }
    }

    fn make_underline(content: &str) -> String {
        let mut output = String::new();
        output.push_str(&format!("{}\n", content));
        output.push_str(&format!("{}\n", "-".repeat(content.len())));
        output
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[fuchsia::test]
    fn action_result_formatter_to_warnings_when_no_actions_triggered() {
        let action_results = ActionResults::new();
        let formatter = ActionResultFormatter::new(&action_results);

        assert_eq!(String::from("No actions were triggered. All targets OK."), formatter.to_text());
    }

    #[fuchsia::test]
    fn action_result_formatter_to_text_when_actions_triggered() {
        let warnings = String::from(
            "Warnings\n\
        --------\n\
        w1\n\
        w2\n",
        );

        let mut action_results = ActionResults::new();
        action_results.warnings.push(String::from("w1"));
        action_results.warnings.push(String::from("w2"));

        let formatter = ActionResultFormatter::new(&action_results);

        assert_eq!(warnings, formatter.to_text());
    }

    #[fuchsia::test]
    fn action_result_formatter_to_text_with_gauges() {
        let warnings = String::from(
            "\
            Featured Values\n\
            ---------------\n\
            g1\n\n\
            Warnings\n\
        --------\n\
        w1\n\
        w2\n\
        ",
        );

        let mut action_results = ActionResults::new();
        action_results.warnings.push(String::from("w1"));
        action_results.warnings.push(String::from("w2"));
        action_results.gauges.push(String::from("g1"));

        let formatter = ActionResultFormatter::new(&action_results);

        assert_eq!(warnings, formatter.to_text());
    }

    #[fuchsia::test]
    fn action_result_formatter_sorts_output() {
        let warnings = String::from(
            "\
            Featured Values\n\
            ---------------\n\
            g1\n\
            g2\n\n\
            Warnings\n\
        --------\n\
        w1\n\
        w2\n\n\
        Crashes Plugin - OK\n\
        Warning Plugin\n\
        Warnings\n\
        --------\n\
        w1\n\
        w2\n\n\
        Gauges Plugin - OK\n\
        Featured Values\n\
        ---------------\n\
        g2\n\
        g1\n\
        ",
        );

        {
            let mut action_results = ActionResults::new();
            action_results.warnings.push(String::from("w1"));
            action_results.warnings.push(String::from("w2"));
            action_results.gauges.push(String::from("g1"));
            action_results.gauges.push(String::from("g2"));
            let mut warnings_plugin = ActionResults::new();
            warnings_plugin.warnings.push(String::from("w1"));
            warnings_plugin.warnings.push(String::from("w2"));
            let mut gauges_plugin = ActionResults::new();
            gauges_plugin.gauges.push(String::from("g2"));
            gauges_plugin.gauges.push(String::from("g1"));
            gauges_plugin.sort_gauges = false;
            action_results
                .sub_results
                .push(("Crashes".to_string(), Box::new(ActionResults::new())));
            action_results.sub_results.push(("Warning".to_string(), Box::new(warnings_plugin)));
            action_results.sub_results.push(("Gauges".to_string(), Box::new(gauges_plugin)));

            let formatter = ActionResultFormatter::new(&action_results);

            assert_eq!(warnings, formatter.to_text());
        }

        // Same as before, but reversed to test sorting.
        {
            let mut action_results = ActionResults::new();
            action_results.warnings.push(String::from("w2"));
            action_results.warnings.push(String::from("w1"));
            action_results.gauges.push(String::from("g2"));
            action_results.gauges.push(String::from("g1"));
            let mut warnings_plugin = ActionResults::new();
            warnings_plugin.warnings.push(String::from("w2"));
            warnings_plugin.warnings.push(String::from("w1"));
            let mut gauges_plugin = ActionResults::new();
            gauges_plugin.gauges.push(String::from("g2"));
            gauges_plugin.gauges.push(String::from("g1"));
            gauges_plugin.sort_gauges = false;
            action_results
                .sub_results
                .push(("Crashes".to_string(), Box::new(ActionResults::new())));
            action_results.sub_results.push(("Warning".to_string(), Box::new(warnings_plugin)));
            action_results.sub_results.push(("Gauges".to_string(), Box::new(gauges_plugin)));

            let formatter = ActionResultFormatter::new(&action_results);

            assert_eq!(warnings, formatter.to_text());
        }
    }

    #[fuchsia::test]
    fn action_result_verbose_works() {
        let readable_warnings = String::from(
            "\
            Featured Values\n\
            ---------------\n\
            g1: 42\n\
            \n\
            Warnings\n\
            --------\n\
            w1\n\
            ",
        );
        let verbose_warnings = format!(
            "{}{}",
            readable_warnings,
            "\n\
            Featured Values (Not Computable)\n\
            --------------------------------\n\
            g2: N/A\n\
            \n\
            Info\n\
            ----\n\
            i1\n\
            "
        );
        let mut action_results = ActionResults::new();
        action_results.warnings.push(String::from("w1"));
        action_results.gauges.push(String::from("g1: 42"));
        action_results.infos.push(String::from("i1"));
        action_results.broken_gauges.push(String::from("g2: N/A"));
        let mut verbose_action_results = action_results.clone();
        verbose_action_results.verbose = true;
        assert_eq!(readable_warnings, ActionResultFormatter::new(&action_results).to_text());
        assert_eq!(verbose_warnings, ActionResultFormatter::new(&verbose_action_results).to_text());
    }
}
