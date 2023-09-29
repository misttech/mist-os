// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_data::{DiagnosticsHierarchy, InspectData};

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct CoredumpReport {
    pub idx: usize,
    pub pid: i64,
    pub argv: String,
}

impl CoredumpReport {
    /// Returns `None` if the snapshot seems partially-written, indicating a retry is warranted.
    ///
    /// # Panics
    ///
    /// If the snapshot has the same rough structure as Starnix's inspect output but was incorrectly
    /// recorded.
    pub fn extract_from_snapshot(data: &InspectData) -> Option<Vec<Self>> {
        let DiagnosticsHierarchy { name, properties, children, .. } = data
            .payload
            .as_ref()?
            .get_child_by_path(&["container", "kernel", "coredumps"])
            .cloned()?;
        assert_eq!(name, "coredumps");
        assert_eq!(properties, vec![]);

        // Examine all of the properties and children so that we are alerted that this test code may
        // need to be updated if coredump reports gain new information.
        let mut reports = vec![];
        for DiagnosticsHierarchy {
            name: idx_str,
            properties: coredump_properties,
            children: coredump_children,
            ..
        } in children
        {
            assert!(
                coredump_children.is_empty(),
                "coredump reports aren't expected to have children, found {coredump_children:?}"
            );

            let mut argv = None;
            let mut pid = None;
            for property in coredump_properties {
                match property.name() {
                    "argv" => {
                        argv = Some(
                            property
                                .string()
                                .expect("getting argv string from report node")
                                .to_string(),
                        )
                    }

                    // TODO(https://fxbug.dev/130834) i64/int in kernel shows up as u64/uint here
                    "pid" => {
                        pid = Some(property.uint().expect("getting pid from report node") as i64)
                    }
                    other => panic!("unrecognized coredump report property `{other}`"),
                }
            }

            reports.push(Self {
                idx: idx_str.parse().expect("starnix coredump node names should be integers"),
                pid: pid.expect("retrieving pid property"),
                argv: argv.expect("retrieving argv property"),
            });
        }
        reports.sort();

        Some(reports)
    }
}
