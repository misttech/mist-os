// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use selinux_policy::{metadata::HandleUnknown, parser::ByValue, Policy};

use serde::Deserialize;
use std::io::{Cursor, Read as _};

const TESTDATA_DIR: &str = env!("TESTDATA_DIR");
const POLICIES_SUBDIR: &str = "policies";
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

        let by_value = ByValue::new(Cursor::new(policy_bytes));

        let policy = Policy::parse(by_value).expect("parse policy");
        policy.validate().expect("validate policy");

        assert_eq!(expectations.expected_policy_version, policy.policy_version());
        assert_eq!(&expectations.expected_handle_unknown, policy.handle_unknown());
    }
}
