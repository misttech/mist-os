// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use selinux_policy::{metadata::HandleUnknown, parse_policy_by_reference, parse_policy_by_value};

use serde::Deserialize;
use std::io::Read as _;

const TESTDATA_DIR: &str = env!("TESTDATA_DIR");
const POLICIES_SUBDIR: &str = "policies";
const MICRO_POLICIES_SUBDIR: &str = "micro_policies";
const EXPECTATIONS_SUBDIR: &str = "expectations";

#[derive(Debug, Deserialize)]
struct Expectations {
    expected_policy_version: u32,
    expected_handle_unknown: LocalHandleUnknown,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum LocalHandleUnknown {
    Deny,
    Reject,
    Allow,
}

impl PartialEq<HandleUnknown> for LocalHandleUnknown {
    fn eq(&self, other: &HandleUnknown) -> bool {
        match self {
            LocalHandleUnknown::Deny => other == &HandleUnknown::Deny,
            LocalHandleUnknown::Reject => other == &HandleUnknown::Reject,
            LocalHandleUnknown::Allow => other == &HandleUnknown::Allow,
        }
    }
}

#[test]
fn known_policies() {
    let policies_dir = format!("{}/{}", TESTDATA_DIR, POLICIES_SUBDIR);
    let expectations_dir = format!("{}/{}", TESTDATA_DIR, EXPECTATIONS_SUBDIR);

    for policy_item in std::fs::read_dir(policies_dir).expect("read testdata policies directory") {
        let policy_path = policy_item.expect("policy file directory item").path();
        let filename = policy_path
            .file_name()
            .expect("policy file name")
            .to_str()
            .expect("policy file name as string");

        let expectations_path = format!("{}/{}", expectations_dir, filename);
        let mut expectations_file = std::fs::File::open(&expectations_path)
            .unwrap_or_else(|_| panic!("open expectations file: {:?}", expectations_path));
        let expectations = serde_json5::from_reader::<Expectations, _>(&mut expectations_file)
            .expect("deserialize expectations");

        let mut policy_file = std::fs::File::open(&policy_path).expect("open policy file");
        let mut policy_bytes = vec![];
        policy_file.read_to_end(&mut policy_bytes).expect("read policy file");

        // Test parse-by-value.

        let (policy, returned_policy_bytes) =
            parse_policy_by_value(policy_bytes.clone()).expect("parse policy");
        let policy = policy.validate().expect("validate policy");

        assert_eq!(expectations.expected_policy_version, policy.policy_version());
        assert_eq!(&expectations.expected_handle_unknown, policy.handle_unknown());

        // Returned policy bytes must be identical to input policy bytes.
        assert_eq!(policy_bytes, returned_policy_bytes);

        // Test parse-by-reference.

        let policy = parse_policy_by_reference(policy_bytes.as_slice()).expect("parse policy");
        let policy = policy.validate().expect("validate policy");

        assert_eq!(expectations.expected_policy_version, policy.policy_version());
        assert_eq!(&expectations.expected_handle_unknown, policy.handle_unknown());
    }
}

#[test]
fn explicit_allow_type_type() {
    let policy_path =
        format!("{}/{}/allow_a_t_b_t_class0_perm0_policy.pp", TESTDATA_DIR, MICRO_POLICIES_SUBDIR);
    let policy_bytes = std::fs::read(&policy_path).expect("read policy from file");
    let policy = parse_policy_by_reference(policy_bytes.as_slice())
        .expect("parse policy")
        .validate()
        .expect("validate policy");
    assert!(policy
        .is_explicitly_allowed("a_t", "b_t", "class0", "perm0")
        .expect("query well-formed"));
}

#[test]
fn no_explicit_allow_type_type() {
    let policy_path = format!(
        "{}/{}/no_allow_a_t_b_t_class0_perm0_policy.pp",
        TESTDATA_DIR, MICRO_POLICIES_SUBDIR
    );
    let policy_bytes = std::fs::read(&policy_path).expect("read policy from file");
    let policy = parse_policy_by_reference(policy_bytes.as_slice())
        .expect("parse policy")
        .validate()
        .expect("validate policy");
    assert!(!policy
        .is_explicitly_allowed("a_t", "b_t", "class0", "perm0")
        .expect("query well-formed"));
}

#[test]
fn explicit_allow_type_attr() {
    let policy_path = format!(
        "{}/{}/allow_a_t_b_attr_class0_perm0_policy.pp",
        TESTDATA_DIR, MICRO_POLICIES_SUBDIR
    );
    let policy_bytes = std::fs::read(&policy_path).expect("read policy from file");
    let policy = parse_policy_by_reference(policy_bytes.as_slice())
        .expect("parse policy")
        .validate()
        .expect("validate policy");
    assert!(policy
        .is_explicitly_allowed("a_t", "b_t", "class0", "perm0")
        .expect("query well-formed"));
}

#[test]
fn no_explicit_allow_type_attr() {
    let policy_path = format!(
        "{}/{}/no_allow_a_t_b_attr_class0_perm0_policy.pp",
        TESTDATA_DIR, MICRO_POLICIES_SUBDIR
    );
    let policy_bytes = std::fs::read(&policy_path).expect("read policy from file");
    let policy = parse_policy_by_reference(policy_bytes.as_slice())
        .expect("parse policy")
        .validate()
        .expect("validate policy");
    assert!(!policy
        .is_explicitly_allowed("a_t", "b_t", "class0", "perm0")
        .expect("query well-formed"));
}

#[test]
fn explicit_allow_attr_attr() {
    let policy_path = format!(
        "{}/{}/allow_a_attr_b_attr_class0_perm0_policy.pp",
        TESTDATA_DIR, MICRO_POLICIES_SUBDIR
    );
    let policy_bytes = std::fs::read(&policy_path).expect("read policy from file");
    let policy = parse_policy_by_reference(policy_bytes.as_slice())
        .expect("parse policy")
        .validate()
        .expect("validate policy");
    assert!(policy
        .is_explicitly_allowed("a_t", "b_t", "class0", "perm0")
        .expect("query well-formed"));
}

#[test]
fn no_explicit_allow_attr_attr() {
    let policy_path = format!(
        "{}/{}/no_allow_a_attr_b_attr_class0_perm0_policy.pp",
        TESTDATA_DIR, MICRO_POLICIES_SUBDIR
    );
    let policy_bytes = std::fs::read(&policy_path).expect("read policy from file");
    let policy = parse_policy_by_reference(policy_bytes.as_slice())
        .expect("parse policy")
        .validate()
        .expect("validate policy");
    assert!(!policy
        .is_explicitly_allowed("a_t", "b_t", "class0", "perm0")
        .expect("query well-formed"));
}
