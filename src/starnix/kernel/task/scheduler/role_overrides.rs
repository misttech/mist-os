// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use itertools::Itertools as _;
use regex::bytes::RegexSet;
use regex::Error;

/// Per-container overrides for thread roles.
#[derive(Debug)]
pub struct RoleOverrides {
    process_filter: RegexSet,
    thread_filter: RegexSet,
    role_names: Vec<String>,
}

impl RoleOverrides {
    /// Create a new builder for role overrides.
    pub fn new() -> RoleOverridesBuilder {
        RoleOverridesBuilder {
            process_patterns: vec![],
            thread_patterns: vec![],
            role_names: vec![],
        }
    }

    /// Get the overridden role name (if any) for provided process and thread names.
    pub fn get_role_name<'a>(
        &self,
        process_name: impl AsRef<[u8]>,
        thread_name: impl AsRef<[u8]>,
    ) -> Option<&str> {
        debug_assert_eq!(self.process_filter.len(), self.role_names.len());
        debug_assert_eq!(self.thread_filter.len(), self.role_names.len());

        // RegexSet iterators are sorted, we can assume they'll be in ascending order.
        let process_match_indices = self.process_filter.matches(process_name.as_ref()).into_iter();
        let thread_match_indices = self.thread_filter.matches(thread_name.as_ref()).into_iter();
        let mut indices_with_both_matching =
            process_match_indices.merge(thread_match_indices).duplicates();
        indices_with_both_matching.next().map(|i| self.role_names[i].as_str())
    }
}

/// Builder for `RoleOverrides`.
pub struct RoleOverridesBuilder {
    process_patterns: Vec<String>,
    thread_patterns: Vec<String>,
    role_names: Vec<String>,
}

impl RoleOverridesBuilder {
    /// Add a new override to the configuration.
    pub fn add(
        &mut self,
        process: impl Into<String>,
        thread: impl Into<String>,
        role_name: impl Into<String>,
    ) {
        self.process_patterns.push(process.into());
        self.thread_patterns.push(thread.into());
        self.role_names.push(role_name.into());
    }

    /// Compile all of the provided regular expressions and return a `RoleOverrides`.
    pub fn build(self) -> Result<RoleOverrides, Error> {
        Ok(RoleOverrides {
            process_filter: RegexSet::new(self.process_patterns)?,
            thread_filter: RegexSet::new(self.thread_patterns)?,
            role_names: self.role_names,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    fn single_pattern() {
        let mut builder = RoleOverrides::new();
        builder.add("process_prefix_.+", "thread_prefix_.+", "replacement_role");
        let mappings = builder.build().unwrap();

        assert_eq!(
            mappings.get_role_name("process_prefix_foo", "thread_prefix_bar"),
            Some("replacement_role")
        );
        assert_eq!(mappings.get_role_name("process_prefix_foo", "non_matching"), None);
        assert_eq!(mappings.get_role_name("non_matching", "process_prefix_bar"), None);
        assert_eq!(mappings.get_role_name("non_matching", "non_matching"), None);
    }

    #[fuchsia::test]
    fn multiple_patterns() {
        let mut builder = RoleOverrides::new();
        builder.add("pre_one.+", "pre_one.+", "replace_one");
        builder.add("pre_two.+", "pre_two.+", "replace_two");
        builder.add("pre_three.+", "pre_three.+", "replace_three");
        builder.add("pre_four.+", "pre_four.+", "replace_four");
        let mappings = builder.build().unwrap();

        assert_eq!(mappings.get_role_name("pre_one_foo", "pre_one_bar"), Some("replace_one"));
        assert_eq!(mappings.get_role_name("pre_one_foo", "non_matching"), None);
        assert_eq!(mappings.get_role_name("non_matching", "pre_one_bar"), None);
        assert_eq!(mappings.get_role_name("non_matching", "non_matching"), None);

        assert_eq!(mappings.get_role_name("pre_two_foo", "pre_two_bar"), Some("replace_two"));
        assert_eq!(mappings.get_role_name("pre_two_foo", "non_matching"), None);
        assert_eq!(mappings.get_role_name("non_matching", "pre_two_bar"), None);

        assert_eq!(mappings.get_role_name("pre_three_foo", "pre_three_bar"), Some("replace_three"));
        assert_eq!(mappings.get_role_name("pre_three_foo", "non_matching"), None);
        assert_eq!(mappings.get_role_name("non_matching", "pre_three_bar"), None);

        assert_eq!(mappings.get_role_name("pre_four_foo", "pre_four_bar"), Some("replace_four"));
        assert_eq!(mappings.get_role_name("pre_four_foo", "non_matching"), None);
        assert_eq!(mappings.get_role_name("non_matching", "pre_four_bar"), None);
    }
}
