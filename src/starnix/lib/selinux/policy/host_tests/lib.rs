// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use selinux_policy::{
    error::NewSecurityContextError, metadata::HandleUnknown, parse_policy_by_reference,
    parse_policy_by_value, SecurityContext,
};

use anyhow::Context as _;
use selinux_common::{FileClass, InitialSid, Permission, ProcessPermission};
use serde::Deserialize;
use std::io::Read as _;

const TESTDATA_DIR: &str = env!("TESTDATA_DIR");
const POLICIES_SUBDIR: &str = "policies";
const MICRO_POLICIES_SUBDIR: &str = "micro_policies";
const COMPOSITE_POLICIES_SUBDIR: &str = "composite_policies";
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

#[cfg(feature = "selinux_policy_test_api")]
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

        let policy = policy
            .validate()
            .with_context(|| format!("policy path: {:?}", policy_path))
            .expect("validate policy");

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
fn policy_lookup() {
    let testsuite_policy_path = format!("{}/{}/selinux_testsuite", TESTDATA_DIR, POLICIES_SUBDIR);
    let mut policy_file =
        std::fs::File::open(&testsuite_policy_path).expect("open selinux testsuite policy file");
    let mut policy_bytes = vec![];
    policy_file.read_to_end(&mut policy_bytes).expect("read selinux testsuite policy file");
    let (policy, _) = parse_policy_by_value(policy_bytes.clone()).expect("parse policy");
    let policy = policy.validate().expect("validate selinux testsuite policy");

    let unconfined_t = policy.type_id_by_name("unconfined_t");

    policy
        .is_explicitly_allowed(
            unconfined_t,
            unconfined_t,
            Permission::Process(ProcessPermission::Fork),
        )
        .expect("check for `allow unconfined_t unconfined_t:process fork;` in policy");
}

#[test]
fn initial_contexts() {
    let policy_path = format!(
        "{}/{}/multiple_levels_and_categories_policy.pp",
        TESTDATA_DIR, MICRO_POLICIES_SUBDIR
    );
    let mut policy_file = std::fs::File::open(&policy_path).expect("open policy file");
    let mut policy_bytes = vec![];
    policy_file.read_to_end(&mut policy_bytes).expect("read policy file");
    let (policy, _) = parse_policy_by_value(policy_bytes.clone()).expect("parse policy");
    let policy = policy.validate().expect("validate policy");

    let kernel_context = policy.initial_context(InitialSid::Kernel);
    assert_eq!(
        policy.serialize_security_context(&kernel_context),
        b"user0:object_r:type0:s0:c0-s1:c0.c2,c4"
    )
}

#[cfg(feature = "selinux_policy_test_api")]
#[test]
fn explicit_allow_type_type() {
    let policy_path =
        format!("{}/{}/allow_a_t_b_t_class0_perm0_policy.pp", TESTDATA_DIR, MICRO_POLICIES_SUBDIR);
    let policy_bytes = std::fs::read(&policy_path).expect("read policy from file");
    let policy = parse_policy_by_reference(policy_bytes.as_slice()).expect("parse policy");
    let parsed_policy = policy.parsed_policy();
    parsed_policy.validate().expect("validate policy");

    let a_t = parsed_policy.type_id_by_name("a_t");
    let b_t = parsed_policy.type_id_by_name("b_t");

    assert!(parsed_policy
        .is_explicitly_allowed_custom(a_t, b_t, "class0", "perm0")
        .expect("query well-formed"));
}

#[cfg(feature = "selinux_policy_test_api")]
#[test]
fn no_explicit_allow_type_type() {
    let policy_path = format!(
        "{}/{}/no_allow_a_t_b_t_class0_perm0_policy.pp",
        TESTDATA_DIR, MICRO_POLICIES_SUBDIR
    );
    let policy_bytes = std::fs::read(&policy_path).expect("read policy from file");
    let policy = parse_policy_by_reference(policy_bytes.as_slice()).expect("parse policy");
    let parsed_policy = policy.parsed_policy();
    parsed_policy.validate().expect("validate policy");

    let a_t = parsed_policy.type_id_by_name("a_t");
    let b_t = parsed_policy.type_id_by_name("b_t");

    assert!(!parsed_policy
        .is_explicitly_allowed_custom(a_t, b_t, "class0", "perm0")
        .expect("query well-formed"));
}

#[cfg(feature = "selinux_policy_test_api")]
#[test]
fn explicit_allow_type_attr() {
    let policy_path = format!(
        "{}/{}/allow_a_t_b_attr_class0_perm0_policy.pp",
        TESTDATA_DIR, MICRO_POLICIES_SUBDIR
    );
    let policy_bytes = std::fs::read(&policy_path).expect("read policy from file");
    let policy = parse_policy_by_reference(policy_bytes.as_slice()).expect("parse policy");
    let parsed_policy = policy.parsed_policy();
    parsed_policy.validate().expect("validate policy");

    let a_t = parsed_policy.type_id_by_name("a_t");
    let b_t = parsed_policy.type_id_by_name("b_t");

    assert!(parsed_policy
        .is_explicitly_allowed_custom(a_t, b_t, "class0", "perm0")
        .expect("query well-formed"));
}

#[cfg(feature = "selinux_policy_test_api")]
#[test]
fn no_explicit_allow_type_attr() {
    let policy_path = format!(
        "{}/{}/no_allow_a_t_b_attr_class0_perm0_policy.pp",
        TESTDATA_DIR, MICRO_POLICIES_SUBDIR
    );
    let policy_bytes = std::fs::read(&policy_path).expect("read policy from file");
    let policy = parse_policy_by_reference(policy_bytes.as_slice()).expect("parse policy");
    let parsed_policy = policy.parsed_policy();
    parsed_policy.validate().expect("validate policy");

    let a_t = parsed_policy.type_id_by_name("a_t");
    let b_t = parsed_policy.type_id_by_name("b_t");

    assert!(!parsed_policy
        .is_explicitly_allowed_custom(a_t, b_t, "class0", "perm0")
        .expect("query well-formed"));
}

#[cfg(feature = "selinux_policy_test_api")]
#[test]
fn explicit_allow_attr_attr() {
    let policy_path = format!(
        "{}/{}/allow_a_attr_b_attr_class0_perm0_policy.pp",
        TESTDATA_DIR, MICRO_POLICIES_SUBDIR
    );
    let policy_bytes = std::fs::read(&policy_path).expect("read policy from file");
    let policy = parse_policy_by_reference(policy_bytes.as_slice()).expect("parse policy");
    let parsed_policy = policy.parsed_policy();
    parsed_policy.validate().expect("validate policy");

    let a_t = parsed_policy.type_id_by_name("a_t");
    let b_t = parsed_policy.type_id_by_name("b_t");

    assert!(parsed_policy
        .is_explicitly_allowed_custom(a_t, b_t, "class0", "perm0")
        .expect("query well-formed"));
}

#[cfg(feature = "selinux_policy_test_api")]
#[test]
fn no_explicit_allow_attr_attr() {
    let policy_path = format!(
        "{}/{}/no_allow_a_attr_b_attr_class0_perm0_policy.pp",
        TESTDATA_DIR, MICRO_POLICIES_SUBDIR
    );
    let policy_bytes = std::fs::read(&policy_path).expect("read policy from file");
    let policy = parse_policy_by_reference(policy_bytes.as_slice()).expect("parse policy");
    let parsed_policy = policy.parsed_policy();
    parsed_policy.validate().expect("validate policy");

    let a_t = parsed_policy.type_id_by_name("a_t");
    let b_t = parsed_policy.type_id_by_name("b_t");

    assert!(!parsed_policy
        .is_explicitly_allowed_custom(a_t, b_t, "class0", "perm0")
        .expect("query well-formed"));
}

#[cfg(feature = "selinux_policy_test_api")]
#[test]
fn compute_explicitly_allowed_multiple_attributes() {
    let policy_path = format!(
        "{}/{}/allow_a_t_a1_attr_class0_perm0_a2_attr_class0_perm1_policy.pp",
        TESTDATA_DIR, MICRO_POLICIES_SUBDIR
    );
    let policy_bytes = std::fs::read(&policy_path).expect("read policy from file");
    let policy = parse_policy_by_reference(policy_bytes.as_slice()).expect("parse policy");
    let parsed_policy = policy.parsed_policy();
    parsed_policy.validate().expect("validate policy");

    let a_t = parsed_policy.type_id_by_name("a_t");

    let raw_access_vector = parsed_policy
        .compute_explicitly_allowed_custom(a_t, a_t, "class0")
        .expect("well-formed query")
        .into_raw();

    // Two separate attributes are each allowed one permission on `[attr] self:class0`. Both
    // attributes are associated with "a_t". No other `allow` statements appear in the policy
    // in relation to "a_t". Therefore, we expect exactly two 1's in the access vector for
    // query `("a_t", "a_t", "class0")`.
    assert_eq!(2, raw_access_vector.count_ones());
}

#[test]
fn new_file_security_context_minimal() {
    let policy_path =
        format!("{}/{}/compiled/minimal_policy.pp", TESTDATA_DIR, COMPOSITE_POLICIES_SUBDIR);
    let policy_bytes = std::fs::read(&policy_path).expect("read policy from file");
    let policy = parse_policy_by_reference(policy_bytes.as_slice())
        .expect("parse policy")
        .validate()
        .expect("validate policy");
    let source = policy
        .parse_security_context("source_u:source_r:source_t:s0:c0-s2:c0.c1".as_bytes())
        .expect("valid source security context");
    let target = policy
        .parse_security_context("target_u:target_r:target_t:s1:c1".as_bytes())
        .expect("valid target security context");

    let actual = policy
        .new_file_security_context(&source, &target, &FileClass::File)
        .expect("compute new context for new file");
    let expected: SecurityContext = policy
        .parse_security_context("source_u:object_r:target_t:s0:c0".as_bytes())
        .expect("valid expected security context");

    assert_eq!(expected, actual);
}

#[test]
fn new_file_security_context_class_defaults() {
    let policy_path =
        format!("{}/{}/compiled/class_defaults_policy.pp", TESTDATA_DIR, COMPOSITE_POLICIES_SUBDIR);
    let policy_bytes = std::fs::read(&policy_path).expect("read policy from file");
    let policy = parse_policy_by_reference(policy_bytes.as_slice())
        .expect("parse policy")
        .validate()
        .expect("validate policy");
    let source = policy
        .parse_security_context("source_u:source_r:source_t:s0:c0-s2:c0.c1".as_bytes())
        .expect("valid source security context");
    let target = policy
        .parse_security_context("target_u:target_r:target_t:s1:c0-s1:c0.c1".as_bytes())
        .expect("valid target security context");

    let actual = policy
        .new_file_security_context(&source, &target, &FileClass::File)
        .expect("compute new context for new file");
    let expected: SecurityContext = policy
        .parse_security_context("target_u:source_r:source_t:s1:c0-s1:c0.c1".as_bytes())
        .expect("valid expected security context");

    assert_eq!(expected, actual);
}

#[test]
fn new_file_security_context_role_transition() {
    let policy_path = format!(
        "{}/{}/compiled/role_transition_policy.pp",
        TESTDATA_DIR, COMPOSITE_POLICIES_SUBDIR
    );
    let policy_bytes = std::fs::read(&policy_path).expect("read policy from file");
    let policy = parse_policy_by_reference(policy_bytes.as_slice())
        .expect("parse policy")
        .validate()
        .expect("validate policy");
    let source = policy
        .parse_security_context("source_u:source_r:source_t:s0:c0-s2:c0.c1".as_bytes())
        .expect("valid source security context");
    let target = policy
        .parse_security_context("target_u:target_r:target_t:s1:c1".as_bytes())
        .expect("valid target security context");

    let actual = policy
        .new_file_security_context(&source, &target, &FileClass::File)
        .expect("compute new context for new file");
    let expected: SecurityContext = policy
        .parse_security_context("source_u:transition_r:target_t:s0:c0".as_bytes())
        .expect("valid expected security context");

    assert_eq!(expected, actual);
}

#[test]
fn new_file_security_context_role_transition_not_allowed() {
    let policy_path = format!(
        "{}/{}/compiled/role_transition_not_allowed_policy.pp",
        TESTDATA_DIR, COMPOSITE_POLICIES_SUBDIR
    );
    let policy_bytes = std::fs::read(&policy_path).expect("read policy from file");
    let policy = parse_policy_by_reference(policy_bytes.as_slice())
        .expect("parse policy")
        .validate()
        .expect("validate policy");
    let source = policy
        .parse_security_context("source_u:source_r:source_t:s0:c0-s2:c0.c1".as_bytes())
        .expect("valid source security context");
    let target = policy
        .parse_security_context("target_u:target_r:target_t:s1:c1".as_bytes())
        .expect("valid target security context");

    let actual =
        policy.new_file_security_context(&source, &target, &FileClass::File).err().expect("error");

    assert_eq!(
        NewSecurityContextError::RoleTransitionNotAllowed {
            source_security_context: source,
            target_security_context: target,
            source_role: "source_r".to_string(),
            new_role: "transition_r".to_string(),
        },
        actual
    );
}

#[test]
fn new_file_security_context_type_transition() {
    let policy_path = format!(
        "{}/{}/compiled/type_transition_policy.pp",
        TESTDATA_DIR, COMPOSITE_POLICIES_SUBDIR
    );
    let policy_bytes = std::fs::read(&policy_path).expect("read policy from file");
    let policy = parse_policy_by_reference(policy_bytes.as_slice())
        .expect("parse policy")
        .validate()
        .expect("validate policy");
    let source = policy
        .parse_security_context("source_u:source_r:source_t:s0:c0-s2:c0.c1".as_bytes())
        .expect("valid source security context");
    let target = policy
        .parse_security_context("target_u:target_r:target_t:s1:c1".as_bytes())
        .expect("valid target security context");

    let actual = policy
        .new_file_security_context(&source, &target, &FileClass::File)
        .expect("compute new context for new file");
    let expected: SecurityContext = policy
        .parse_security_context("source_u:object_r:transition_t:s0:c0".as_bytes())
        .expect("valid expected security context");

    assert_eq!(expected, actual);
}

#[test]
fn new_file_security_context_range_transition() {
    let policy_path = format!(
        "{}/{}/compiled/range_transition_policy.pp",
        TESTDATA_DIR, COMPOSITE_POLICIES_SUBDIR
    );
    let policy_bytes = std::fs::read(&policy_path).expect("read policy from file");
    let policy = parse_policy_by_reference(policy_bytes.as_slice())
        .expect("parse policy")
        .validate()
        .expect("validate policy");
    let source = policy
        .parse_security_context("source_u:source_r:source_t:s0:c0-s2:c0.c1".as_bytes())
        .expect("valid source security context");
    let target = policy
        .parse_security_context("target_u:target_r:target_t:s1:c1".as_bytes())
        .expect("valid target security context");

    let actual = policy
        .new_file_security_context(&source, &target, &FileClass::File)
        .expect("compute new context for new file");
    let expected: SecurityContext = policy
        .parse_security_context("source_u:object_r:target_t:s1:c1-s2:c1.c2".as_bytes())
        .expect("valid expected security context");

    assert_eq!(expected, actual);
}
