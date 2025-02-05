// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::errors::{VerifyError, VerifyErrors, VerifyFailureReason, VerifySource};
use fuchsia_inspect as finspect;
use std::time::Duration;

fn reason_to_string(reason: &VerifyFailureReason) -> &'static str {
    match reason {
        VerifyFailureReason::Fidl(_) => "fidl",
        VerifyFailureReason::Verify(_) => "verify",
        VerifyFailureReason::Timeout => "timeout",
    }
}
fn source_to_string(source: &VerifySource) -> &'static str {
    match source {
        VerifySource::Blobfs => "blobfs",
        VerifySource::Netstack => "netstack",
    }
}

/// Updates inspect state based on the result of the verifications. The inspect hierarchy is
/// constructed in a way that's compatible with Lapis.
pub(super) fn write_to_inspect(
    node: &finspect::Node,
    res: &Result<(), VerifyErrors>,
    total_duration: Duration,
) {
    // We need to convert duration to a u64 because that's the largest size integer that inspect
    // can accept. This is safe because duration will be max 1 hour, which fits into u64.
    // For context, 2^64 microseconds is over 5000 centuries.
    let total_duration_u64 = total_duration.as_micros().try_into().unwrap_or(u64::MAX);

    match res {
        Ok(()) => node.record_child("ota_verification_duration", |duration_node| {
            duration_node.record_uint("success", total_duration_u64)
        }),
        Err(VerifyErrors::VerifyErrors(errors)) => {
            node.record_child("ota_verification_duration", |duration_node| {
                for VerifyError::VerifyError(source, _reason, duration) in errors {
                    let duration_u64 = duration.as_micros().try_into().unwrap_or(u64::MAX);

                    duration_node
                        .record_uint(format!("failure_{}", source_to_string(source)), duration_u64);
                }
            });
            node.record_child("ota_verification_failure", |reason_node| {
                for VerifyError::VerifyError(source, reason, _duration) in errors {
                    reason_node.record_uint(
                        format!("{}_{}", source_to_string(source), reason_to_string(reason)),
                        1,
                    );
                }
            });
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use diagnostics_assertions::assert_data_tree;
    use fidl_fuchsia_update_verify as verify;
    use fuchsia_inspect::Inspector;
    use proptest::prelude::*;

    #[test]
    fn success() {
        let inspector = Inspector::default();

        let () = write_to_inspect(inspector.root(), &Ok(()), Duration::from_micros(2));

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
    fn failure_blobfs_fidl() {
        let inspector = Inspector::default();

        let () = write_to_inspect(
            inspector.root(),
            &Err(VerifyErrors::VerifyErrors(vec![VerifyError::VerifyError(
                VerifySource::Blobfs,
                VerifyFailureReason::Fidl(fidl::Error::ExtraBytes),
                Duration::from_micros(2),
            )])),
            Duration::from_micros(4),
        );

        assert_data_tree! {
            inspector,
            root: {
                "ota_verification_duration": {
                    "failure_blobfs" : 2u64,
                },
                "ota_verification_failure": {
                    "blobfs_fidl": 1u64,
                }
            }
        }
    }

    #[test]
    fn failure_blobfs_timeout() {
        let inspector = Inspector::default();

        let () = write_to_inspect(
            inspector.root(),
            &Err(VerifyErrors::VerifyErrors(vec![VerifyError::VerifyError(
                VerifySource::Blobfs,
                VerifyFailureReason::Timeout,
                Duration::from_micros(2),
            )])),
            Duration::from_micros(4),
        );

        assert_data_tree! {
            inspector,
            root: {
                "ota_verification_duration": {
                    "failure_blobfs" : 2u64,
                },
                "ota_verification_failure": {
                    "blobfs_timeout": 1u64,
                }
            }
        }
    }

    #[test]
    fn failure_blobfs_verify() {
        let inspector = Inspector::default();

        let () = write_to_inspect(
            inspector.root(),
            &Err(VerifyErrors::VerifyErrors(vec![VerifyError::VerifyError(
                VerifySource::Blobfs,
                VerifyFailureReason::Verify(verify::VerifyError::Internal),
                Duration::from_micros(2),
            )])),
            Duration::from_micros(4),
        );

        assert_data_tree! {
            inspector,
            root: {
                "ota_verification_duration": {
                    "failure_blobfs" : 2u64,
                },
                "ota_verification_failure": {
                    "blobfs_verify": 1u64,
                }
            }
        }
    }

    #[test]
    fn failure_blobfs_and_netstack() {
        let inspector = Inspector::default();

        let () = write_to_inspect(
            inspector.root(),
            &Err(VerifyErrors::VerifyErrors(vec![
                VerifyError::VerifyError(
                    VerifySource::Blobfs,
                    VerifyFailureReason::Verify(verify::VerifyError::Internal),
                    Duration::from_micros(2),
                ),
                VerifyError::VerifyError(
                    VerifySource::Netstack,
                    VerifyFailureReason::Timeout,
                    Duration::from_micros(999),
                ),
            ])),
            Duration::from_micros(4),
        );

        assert_data_tree! {
            inspector,
            root: {
                "ota_verification_duration": {
                    "failure_blobfs" : 2u64,
                    "failure_netstack" : 999u64,
                },
                "ota_verification_failure": {
                    "blobfs_verify": 1u64,
                    "netstack_timeout": 1u64,
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
                write_to_inspect(inspector.root(), &Ok(()), Duration::new(u64::MAX, nanos));

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
