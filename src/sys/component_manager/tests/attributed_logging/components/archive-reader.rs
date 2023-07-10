// Copyright 2020 the Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use tracing::info;
use {
    diagnostics_data::Logs, diagnostics_reader::ArchiveReader, futures::stream::StreamExt,
    std::collections::HashMap, std::vec::Vec,
};

#[fuchsia::main(logging_tags = ["archive-reader"])]
async fn main() {
    let reader = ArchiveReader::new();
    let mut non_matching_logs = vec![];

    type Fingerprint = Vec<&'static str>;
    let mut treasure = HashMap::<String, Vec<Fingerprint>>::new();
    treasure.insert(
        "routing-tests/offers-to-children-unavailable/child-for-offer-from-parent".to_string(),
        vec![vec![
            "Required",
            "protocol `fidl.test.components.Trigger`",
            "not available for target component \
            `root/routing-tests/offers-to-children-unavailable/child-for-offer-from-parent`",
            "`fidl.test.components.Trigger` was not offered to",
            "`root/routing-tests/offers-to-children-unavailable` by parent",
        ]],
    );
    treasure.insert(
        "routing-tests/offers-to-children-unavailable-but-optional/child-for-offer-from-parent".to_string(),
        vec![vec![
            "Optional",
            "protocol `fidl.test.components.Trigger`",
            "not available for target component \
            `root/routing-tests/offers-to-children-unavailable-but-optional/child-for-offer-from-parent`",
            "Target optionally uses capability that was not available",
            "`fidl.test.components.Trigger` was not offered to",
            "`root/routing-tests/offers-to-children-unavailable-but-optional` by parent",
        ]],
    );
    treasure.insert(
        "routing-tests/child".to_string(),
        vec![vec![
            "Required",
            "protocol `fidl.test.components.Trigger`",
            "not available for target component \
            `root/routing-tests/child`",
            "`fidl.test.components.Trigger` was not offered to",
            "`root/routing-tests/child` by parent",
        ]],
    );
    treasure.insert(
        "routing-tests/child-with-optional-use".to_string(),
        vec![vec![
            "Optional",
            "protocol `fidl.test.components.Trigger`",
            "not available for target component \
            `root/routing-tests/child-with-optional-use`",
            "Target optionally uses capability that was not available",
            "`fidl.test.components.Trigger` was not offered to",
            "`root/routing-tests/child-with-optional-use` by parent",
        ]],
    );
    treasure.insert(
        "routing-tests/offers-to-children-unavailable/child-for-offer-from-sibling".to_string(),
        vec![vec![
            "Required",
            "protocol `fidl.test.components.Trigger`",
            "not available for target component \
            `root/routing-tests/offers-to-children-unavailable/child-for-offer-from-sibling`",
            "`fidl.test.components.Trigger` was not exposed to `root/routing-tests/offers-to-children-unavailable`",
            "from child `#child-that-doesnt-expose`"
        ]],
    );
    treasure.insert(
        "routing-tests/offers-to-children-unavailable-but-optional/child-for-offer-from-sibling".to_string(),
        vec![vec![
            "Optional",
            "protocol `fidl.test.components.Trigger`",
            "not available for target component \
            `root/routing-tests/offers-to-children-unavailable-but-optional/child-for-offer-from-sibling`",
            "Target optionally uses capability that was not available",
            "`fidl.test.components.Trigger` was not exposed to `root/routing-tests/offers-to-children-unavailable-but-optional`",
            "from child `#child-that-doesnt-expose`"
        ]],
    );
    treasure.insert(
        "routing-tests/offers-to-children-unavailable/child-open-unrequested".to_string(),
        vec![vec![
            "No capability available",
            "fidl.test.components.Trigger",
            "root/routing-tests/offers-to-children-unavailable/child-open-unrequested",
            "`use` declaration",
        ]],
    );
    treasure.insert(
        "routing-tests/offers-to-children-unavailable-but-optional/child-open-unrequested"
            .to_string(),
        vec![vec![
            "No capability available",
            "fidl.test.components.Trigger",
            "root/routing-tests/offers-to-children-unavailable-but-optional/child-open-unrequested",
            "`use` declaration",
        ]],
    );

    if let Ok(mut results) = reader.snapshot_then_subscribe::<Logs>() {
        while let Some(Ok(log_record)) = results.next().await {
            if let Some(log_str) = log_record.msg() {
                info!("Log from {}: {}", log_record.moniker, log_str);
                match treasure.get_mut(&log_record.moniker) {
                    None => non_matching_logs.push(log_record),
                    Some(log_fingerprints) => {
                        let removed = {
                            let print_count = log_fingerprints.len();
                            log_fingerprints.retain(|fingerprint| {
                                // If all the part of the fingerprint match, remove
                                // the fingerprint, otherwise keep it.
                                let has_all_features =
                                    fingerprint.iter().all(|feature| log_str.contains(feature));
                                !has_all_features
                            });

                            print_count != log_fingerprints.len()
                        };

                        // If there are no more fingerprint sets for this
                        // component, remove it
                        if log_fingerprints.is_empty() {
                            treasure.remove(&log_record.moniker);
                        }
                        // If we didn't remove any fingerprints, this log didn't
                        // match anything, so push it into the non-matching logs.
                        if !removed {
                            non_matching_logs.push(log_record);
                        }
                        if treasure.is_empty() {
                            return;
                        }
                    }
                }
            }
        }
    }
    panic!(
        "One or more logs were not found, remaining fingerprints: {:?}\n\n
        These log records were read, but did not match any fingerprints: {:?}",
        treasure, non_matching_logs
    );
}
