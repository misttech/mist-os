// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::policy::parser::ByValue;
use crate::policy::{Policy, TypeId};
use crate::ObjectClass;

use anyhow::{anyhow, bail};
use std::collections::HashMap;
use std::num::NonZeroU64;

/// Encapsulates a set of access-check exceptions parsed from a supplied configuration.
pub(super) struct ExceptionsConfig {
    entries: HashMap<ExceptionsEntry, NonZeroU64>,
}

impl ExceptionsConfig {
    /// Parses the supplied `exceptions_config` and returns an `ExceptionsConfig` with an entry for
    /// each parsed exception definition. If a definition's source or target type/domain are not
    /// defined by the supplied `policy` then the entry is ignored, so that removal/renaming of
    /// policy elements will not break the exceptions configuration.
    pub(super) fn new(
        policy: &Policy<ByValue<Vec<u8>>>,
        exceptions_config: &str,
    ) -> Result<Self, anyhow::Error> {
        let lines = exceptions_config.lines();
        let mut result = Self { entries: HashMap::with_capacity(lines.clone().count()) };
        for line in lines {
            result.parse_config_line(policy, line)?;
        }
        result.entries.shrink_to_fit();
        Ok(result)
    }

    /// Returns the non-zero integer bug Id for the exception associated with the specified source,
    /// target and class, if any.
    pub(super) fn lookup(
        &self,
        source: TypeId,
        target: TypeId,
        class: ObjectClass,
    ) -> Option<NonZeroU64> {
        self.entries.get(&ExceptionsEntry { source, target, class }).copied()
    }

    fn parse_config_line(
        &mut self,
        policy: &Policy<ByValue<Vec<u8>>>,
        line: &str,
    ) -> Result<(), anyhow::Error> {
        let mut parts = line.trim().split_whitespace();
        if let Some(statement) = parts.next() {
            match statement {
                "todo_deny" => {
                    // "todo_deny" lines have the form:
                    //   todo_deny b/<id> <source> <target> <class>

                    // Parse the bug Id, which must be present, prefixed by "b/"
                    let bug = parts.next().ok_or_else(|| anyhow!("Expected bug identifier"))?;
                    let bug_id_part = bug.strip_prefix("b/").or_else(|| bug.strip_prefix("https://fxbug.dev/"))
                        .ok_or_else(|| anyhow!("Expected bug Identifier of the form b/<id> or https://fxbug.dev/<id>"))?;
                    let bug_id = bug_id_part
                        .parse::<NonZeroU64>()
                        .map_err(|_| anyhow!("Malformed bug Id: {}", bug_id_part))?;

                    // Parse the source & target types. If either of these is not defined by the
                    // `policy` then the statement is ignored.
                    let stype = policy.type_id_by_name(
                        parts.next().ok_or_else(|| anyhow!("Expected source type"))?,
                    );
                    let ttype = policy.type_id_by_name(
                        parts.next().ok_or_else(|| anyhow!("Expected target type"))?,
                    );

                    // Parse the kernel object class. This must correspond to a known kernel object
                    // class, regardless of whether the policy actually defines the class.
                    let class = parts
                        .next()
                        .and_then(object_class_by_name)
                        .ok_or_else(|| anyhow!("Target class missing or unrecognized"))?;

                    if let (Some(source), Some(target)) = (stype, ttype) {
                        self.entries.insert(ExceptionsEntry { source, target, class }, bug_id);
                    } else {
                        println!("Ignoring statement: {}", line);
                    }
                }
                "" | "//" => {}
                _ => bail!("Unknown statement {}", statement),
            }
        }
        Ok(())
    }
}

/// Key used to index the access check exceptions table.
#[derive(Eq, Hash, PartialEq)]
struct ExceptionsEntry {
    source: TypeId,
    target: TypeId,
    class: ObjectClass,
}

/// Returns the kernel `ObjectClass` corresponding to the supplied `name`, if any.
/// `None` is returned if no such kernel object class exists in the Starnix implementation.
fn object_class_by_name(name: &str) -> Option<ObjectClass> {
    ObjectClass::all_variants().into_iter().find(|class| class.name() == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::parse_policy_by_value;
    use std::sync::Arc;

    const TEST_POLICY: &[u8] =
        include_bytes!("../testdata/composite_policies/compiled/exceptions_config_policy.pp");

    const EXCEPTION_SOURCE_TYPE: &str = "test_exception_source_t";
    const EXCEPTION_TARGET_TYPE: &str = "test_exception_target_t";
    const _EXCEPTION_OTHER_TYPE: &str = "test_exception_other_t";
    const UNMATCHED_TYPE: &str = "test_exception_unmatched_t";

    struct TestData {
        policy: Arc<Policy<ByValue<Vec<u8>>>>,
        defined_source: TypeId,
        defined_target: TypeId,
        unmatched_type: TypeId,
    }

    fn test_data() -> TestData {
        let (parsed, _) = parse_policy_by_value(TEST_POLICY.to_vec()).unwrap();
        let policy = Arc::new(parsed.validate().unwrap());
        let defined_source = policy.type_id_by_name(EXCEPTION_SOURCE_TYPE).unwrap();
        let defined_target = policy.type_id_by_name(EXCEPTION_TARGET_TYPE).unwrap();
        let unmatched_type = policy.type_id_by_name(UNMATCHED_TYPE).unwrap();

        assert!(policy.type_id_by_name("test_undefined_source_t").is_none());
        assert!(policy.type_id_by_name("test_undefined_target_t").is_none());

        TestData { policy, defined_source, defined_target, unmatched_type }
    }

    #[test]
    fn empty_config_is_valid() {
        let _ = ExceptionsConfig::new(&test_data().policy, "")
            .expect("Empty exceptions config is valid");
    }

    #[test]
    fn comments_and_empty_lines_are_valid() {
        let _ = ExceptionsConfig::new(
            &test_data().policy,
            "
            // This is a comment.

            // This is a second comment, with a blank line preceding it.
            ",
        )
        .expect("Config with only comments is valid");
    }

    #[test]
    fn extra_separating_whitespace_is_valid() {
        let _ = ExceptionsConfig::new(
            &test_data().policy,
            "
            todo_deny b/001\ttest_exception_source_t     test_exception_target_t   file
            ",
        )
        .expect("Config with extra separating whitespace is valid");
    }

    const TEST_CONFIG: &str = "
            // These statement should all be resolved.
            todo_deny b/001 test_exception_source_t test_exception_target_t file
            todo_deny b/002 test_exception_other_t test_exception_target_t chr_file
            todo_deny b/003 test_exception_source_t test_exception_other_t anon_inode

            // These statements should not be resolved.
            todo_deny b/101 test_undefined_source_t test_exception_target_t file
            todo_deny b/102 test_exception_source_t test_undefined_target_t file
        ";

    #[test]
    fn only_defined_types_resolve_to_lookup_entries() {
        let test_data = test_data();

        let config = ExceptionsConfig::new(&test_data.policy, TEST_CONFIG)
            .expect("Config with unresolved types is valid");

        assert_eq!(config.entries.len(), 3);
    }

    #[test]
    fn lookup_matching() {
        let test_data = test_data();

        let config = ExceptionsConfig::new(&test_data.policy, TEST_CONFIG)
            .expect("Config with unresolved types is valid");

        // Matching source, target & class will resolve to the corresponding bug Id.
        assert_eq!(
            config.lookup(test_data.defined_source, test_data.defined_target, ObjectClass::File),
            Some(NonZeroU64::new(1).unwrap())
        );

        // Mismatched class, source or target returns no Id.
        assert_eq!(
            config.lookup(test_data.defined_source, test_data.defined_target, ObjectClass::Dir),
            None
        );
        assert_eq!(
            config.lookup(test_data.unmatched_type, test_data.defined_target, ObjectClass::File),
            None
        );
        assert_eq!(
            config.lookup(test_data.defined_source, test_data.unmatched_type, ObjectClass::File),
            None
        );
    }
}
