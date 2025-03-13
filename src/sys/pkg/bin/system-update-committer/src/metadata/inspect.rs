// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_inspect as finspect;
use std::time::Duration;

/// Updates inspect state based on the result of the verifications. The inspect hierarchy is
/// constructed in a way that's compatible with Lapis.
pub(super) fn write_to_inspect(
    node: &finspect::Node,
    res: &Result<zx::Status, fidl::Error>,
    total_duration: Duration,
) {
    // We need to convert duration to a u64 because that's the largest size integer that inspect
    // can accept. This is safe because duration will be max 1 hour, which fits into u64.
    // For context, 2^64 microseconds is over 5000 centuries.
    let total_duration_u64 = total_duration.as_micros().try_into().unwrap_or(u64::MAX);

    match res {
        Ok(zx::Status::OK) => node.record_child("ota_verification_duration", |duration_node| {
            duration_node.record_uint("success", total_duration_u64)
        }),
        Ok(_health_check_error) => {
            node.record_child("ota_verification_duration", |duration_node| {
                duration_node.record_uint("failure_health_check", total_duration_u64);
            });
            node.record_child("ota_verification_failure", |reason_node| {
                reason_node.record_uint("verify", 1);
            });
        }
        Err(_fidl_error) => {
            node.record_child("ota_verification_duration", |duration_node| {
                duration_node.record_uint("failure_health_check", total_duration_u64);
            });
            node.record_child("ota_verification_failure", |reason_node| {
                reason_node.record_uint("fidl", 1);
            });
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::assert_data_tree;
    use fuchsia_inspect::Inspector;
    use proptest::prelude::*;

    #[test]
    fn success() {
        let inspector = Inspector::default();

        let () = write_to_inspect(inspector.root(), &Ok(zx::Status::OK), Duration::from_micros(2));

        assert_data_tree! {
            inspector,
            root: {
                "ota_verification_duration": {
                    "success" : 2u64,
                }
            }
        }
    }

    #[test]
    fn failure_verify() {
        let inspector = Inspector::default();

        let () =
            write_to_inspect(inspector.root(), &Ok(zx::Status::INTERNAL), Duration::from_micros(2));

        assert_data_tree! {
            inspector,
            root: {
                "ota_verification_duration": {
                    "failure_health_check" : 2u64,
                },
                "ota_verification_failure": {
                    "verify": 1u64,
                }
            }
        }
    }

    #[test]
    fn failure_fidl() {
        let inspector = Inspector::default();

        let () = write_to_inspect(
            inspector.root(),
            &Err(fidl::Error::Invalid),
            Duration::from_micros(2),
        );

        assert_data_tree! {
            inspector,
            root: {
                "ota_verification_duration": {
                    "failure_health_check" : 2u64,
                },
                "ota_verification_failure": {
                    "fidl": 1u64,
                }
            }
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig{
            failure_persistence: None,
            ..Default::default()
        })]

         /// Check the largest reported duration is u64::MAX, even if the actual duration is longer.
        #[test]
        fn success_duration_max_u64(nanos in 0u32..1_000_000_000) {
            let inspector = Inspector::default();

            let () =
                write_to_inspect(inspector.root(), &Ok(zx::Status::OK), Duration::new(u64::MAX, nanos));

            assert_data_tree! {
                inspector,
                root: {
                    "ota_verification_duration": {
                        "success" : u64::MAX,
                    }
                }
            }
        }
    }
}
