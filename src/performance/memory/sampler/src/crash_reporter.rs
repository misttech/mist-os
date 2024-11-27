// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module provides an asynchronous service that can be used to
//! regularly file crash reports via the `CrashReporter` FIDL API.

use anyhow::{format_err, Context, Error};
use fidl_fuchsia_feedback::{
    Attachment, CrashReport, CrashReporterMarker, CrashReporterProxy,
    CrashReportingProductRegisterMarker, MAX_NUM_ATTACHMENTS_PER_CRASH_REPORT,
};
use fidl_fuchsia_mem::Buffer;
use fuchsia_async::{Interval, Task};
use fuchsia_component::client::connect_to_protocol;
use futures::channel::mpsc;
use futures::stream::once;
use futures::{StreamExt, TryFutureExt, TryStreamExt};
use itertools::Itertools;
use zx::{MonotonicDuration, Vmo};

const CRASH_PRODUCT_NAME: &str = "FuchsiaHeapProfile";
const CRASH_PROGRAM_NAME: &str = "memory_sampler";
const CRASH_SIGNATURE: &str = "fuchsia-memory-profile";
const MAX_CONCURRENT_PROFILES: usize = 10;
const MAX_SNAPSHOT_RATE_PER_HOUR: i64 = 1;

#[derive(PartialEq, Debug)]
pub enum ProfileReport {
    Partial { process_name: String, profile: Vmo, size: u64, iteration: usize },
    Final { process_name: String, profile: Vmo, size: u64 },
}

impl ProfileReport {
    pub fn get_process_name(&self) -> &str {
        match self {
            ProfileReport::Partial { process_name, .. } => process_name,
            ProfileReport::Final { process_name, .. } => process_name,
        }
    }
}

/// File a crash report with the provided profiles attached to it.
///
/// Warning: Care should be taken to not call this function too often,
/// as to not spam the crash reporting system. As a safe baseline, aim
/// to never file more than a crash report per hour.
///
/// Note: Attached final profiles, are grouped by process name, then
/// numbered. E.g. if there are two final profiles for "process 1" and
/// one profile for "process 2", then they will be named "process
/// 1_final_0", "process 1_final_1" and "process 2_final_0". Partial
/// profiles are numbered by their index; currently no further effort
/// is done to distinguish partial profiles from different processes
/// that share the same name.
///
/// Warning: panics if called with more than
/// `MAX_NUM_ATTACHMENTS_PER_CRASH_REPORT` profiles.
async fn file_report(
    profiles: Vec<ProfileReport>,
    crash_reporter: &CrashReporterProxy,
) -> Result<(), Error> {
    assert!(profiles.len() <= MAX_NUM_ATTACHMENTS_PER_CRASH_REPORT as usize);
    if profiles.is_empty() {
        return Ok(());
    }
    let attachments: Result<Vec<Attachment>, Error> = profiles
        .into_iter()
        .group_by(|p| p.get_process_name().to_string())
        .into_iter()
        .flat_map(|(_, profiles)| profiles.enumerate())
        .map(|(i, profile_report)| {
            let (key, profile, size) = match profile_report {
                ProfileReport::Final { process_name, profile, size } => {
                    (format!("{}_final_{}", process_name, i), profile, size)
                }
                ProfileReport::Partial { process_name, profile, size, iteration } => {
                    (format!("{}_partial_{}", process_name, iteration), profile, size)
                }
            };
            Ok(Attachment { key, value: Buffer { vmo: profile, size } })
        })
        .collect();
    let crash_report = CrashReport {
        program_name: Some(CRASH_PROGRAM_NAME.to_string()),
        crash_signature: Some(CRASH_SIGNATURE.to_string()),
        attachments: Some(attachments?),
        is_fatal: Some(false),
        ..Default::default()
    };
    crash_reporter.file_report(crash_report).await?.map_err(|e| format_err!("{:?}", e))?;
    Ok(())
}

/// Register an appropriate crash product; it will be inherited by
/// every profiling crash report filed with the same program name.
pub async fn register_crash_product() -> Result<(), Error> {
    let crash_reporting_product_register =
        connect_to_protocol::<CrashReportingProductRegisterMarker>()
            .context("Failed to connect to crash reporter service")?;
    let product_config = fidl_fuchsia_feedback::CrashReportingProduct {
        name: Some(CRASH_PRODUCT_NAME.to_string()),
        ..Default::default()
    };
    crash_reporting_product_register.upsert_with_ack(CRASH_PROGRAM_NAME, &product_config).await?;
    Ok(())
}

/// Returns a channel `Sender`, and a task that, when run, will file
/// crash reports pushed to this channel at most
/// `MAX_SNAPSHOT_RATE_PER_HOUR` every hour.
///
/// Note: the returned `Sender` is bounded: trying to queue a message
/// when it is full will cause an error.
pub fn setup_crash_reporter() -> (mpsc::Sender<ProfileReport>, Task<Result<(), Error>>) {
    let throttler_stream = once(std::future::ready(()))
        .chain(Interval::new(MonotonicDuration::from_hours(MAX_SNAPSHOT_RATE_PER_HOUR)));
    let (tx, rx) = mpsc::channel::<ProfileReport>(MAX_CONCURRENT_PROFILES);
    let crash_reporter_task = Task::spawn({
        rx.ready_chunks(MAX_NUM_ATTACHMENTS_PER_CRASH_REPORT as usize)
            .zip(throttler_stream)
            .map(|(chunked_profiles, _)| Ok(chunked_profiles))
            .try_for_each(|profiles| async move {
                let crash_reporter = connect_to_protocol::<CrashReporterMarker>()?;
                file_report(profiles, &crash_reporter)
                    .inspect_err(|e| tracing::error!("Filing report: {}", e))
                    .await
            })
    });
    (tx, crash_reporter_task)
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl::endpoints::create_proxy_and_stream;
    use fidl_fuchsia_feedback::CrashReporterRequest;
    use futures::{try_join, FutureExt};
    use itertools::assert_equal;

    fn handle_crash_reporter_request(request: CrashReporterRequest) -> CrashReport {
        match request {
            CrashReporterRequest::FileReport { report, responder } => {
                let _ = responder.send(Ok(&Default::default()));
                report
            }
        }
    }

    /// Converts an `Attachment` back to a `ProfileReport`.
    /// Note: this function assumes the following format for `Attachment::key`:
    ///   - `<process_name>_final_<index>` if it is a complete report.
    ///   - `<process_name>_partial_<iteration>` if it is a partial report.
    fn retrieve_profile_from_attachment(attachment: Attachment) -> ProfileReport {
        let Attachment { key, value, .. } = attachment;
        let profile = value.vmo;
        let size = value.size;
        let (prefix_key, index) = key.rsplit_once('_').unwrap();
        let (process_name, suffix) = prefix_key.rsplit_once('_').unwrap();
        let is_final = match suffix {
            "final" => true,
            "partial" => false,
            _ => panic!("Unexpected attachment name: {}", key),
        };
        if is_final {
            ProfileReport::Final { process_name: process_name.to_string(), profile, size }
        } else {
            ProfileReport::Partial {
                process_name: process_name.to_string(),
                profile,
                size,
                iteration: index.parse().unwrap(),
            }
        }
    }

    #[fuchsia::test]
    async fn test_file_report_no_profile() {
        let (client, _) = create_proxy_and_stream::<CrashReporterMarker>();
        let no_profiles = vec![];
        let file_report_future = file_report(no_profiles, &client);
        file_report_future
            .now_or_never()
            .expect("Future was not instantly resolved.")
            .expect("Unexpected error when trying to file an empty report.")
    }

    /// Create a non-empty VMO filled with arbitrary data; can be used
    /// for integrity tests.
    fn create_vmo_with_some_data(size: usize) -> Vmo {
        let data: &[u8] = &vec![42; size][..];
        let vmo = Vmo::create(size as u64).unwrap();
        vmo.write(&data, 0).unwrap();
        vmo
    }

    #[fuchsia::test]
    async fn test_file_report_complete_reports() {
        let (client, mut request_stream) = create_proxy_and_stream::<CrashReporterMarker>();

        // Create some profiles with some arbitrary data.
        let size = 42;
        let profiles_metadata = vec![
            // First profile report for a first process.
            ("test_process_1".to_string(), size),
            // First profile report for a second process.
            ("test_process_2".to_string(), size),
            // Second profile report for the first process.
            ("test_process_1".to_string(), size),
        ];
        let profiles = profiles_metadata
            .clone()
            .into_iter()
            .map(|(process_name, size)| ProfileReport::Final {
                process_name,
                size,
                profile: create_vmo_with_some_data(size as usize),
            })
            .collect();
        let file_report_future = file_report(profiles, &client);
        let (_, report) = try_join!(
            file_report_future,
            request_stream
                .next()
                .map(|item| Ok(handle_crash_reporter_request(item.unwrap().unwrap())))
        )
        .unwrap();

        assert_eq!(report.program_name, Some(CRASH_PROGRAM_NAME.to_string()));
        assert_eq!(report.crash_signature, Some(CRASH_SIGNATURE.to_string()));
        let attachments: Vec<(String, Vec<u8>, u64)> = report
            .attachments
            .unwrap()
            .into_iter()
            .map(retrieve_profile_from_attachment)
            .map(|p| match p {
                ProfileReport::Final { process_name, profile, size } => {
                    (process_name, profile.read_to_vec(0, size).unwrap(), size)
                }
                _ => panic!("Expected a complete report, but received a partial report instead."),
            })
            .collect();
        assert_equal(
            attachments.into_iter(),
            profiles_metadata.into_iter().map(|(process_name, size)| {
                (
                    process_name,
                    create_vmo_with_some_data(size as usize).read_to_vec(0, size).unwrap(),
                    size,
                )
            }),
        );
    }

    #[fuchsia::test]
    async fn test_file_report_partial_reports() {
        let (client, mut request_stream) = create_proxy_and_stream::<CrashReporterMarker>();

        // Create some profiles with some arbitrary data.
        let size = 42;
        let profiles_metadata = vec![
            // First profile report for a first process.
            ("test_process_1".to_string(), size),
            // First profile report for a second process.
            ("test_process_2".to_string(), size),
            // Second profile report for the first process.
            ("test_process_1".to_string(), size),
        ];
        let profiles = profiles_metadata
            .clone()
            .into_iter()
            .enumerate()
            .map(|(iteration, (process_name, size))| ProfileReport::Partial {
                process_name,
                size,
                profile: create_vmo_with_some_data(size as usize),
                iteration,
            })
            .collect();

        let file_report_future = file_report(profiles, &client);
        let (_, report) = try_join!(
            file_report_future,
            request_stream
                .next()
                .map(|item| Ok(handle_crash_reporter_request(item.unwrap().unwrap())))
        )
        .unwrap();

        assert_eq!(report.program_name, Some(CRASH_PROGRAM_NAME.to_string()));
        assert_eq!(report.crash_signature, Some(CRASH_SIGNATURE.to_string()));
        let attachments: Vec<(String, Vec<u8>, u64, usize)> = report
            .attachments
            .unwrap()
            .into_iter()
            .map(retrieve_profile_from_attachment)
            .map(|p| match p {
                ProfileReport::Partial { process_name, profile, size, iteration } => {
                    (process_name, profile.read_to_vec(0, size).unwrap(), size, iteration)
                }
                _ => panic!("Expected a partial report, but received a complete report instead."),
            })
            .collect();
        assert_equal(
            attachments.into_iter(),
            profiles_metadata.clone().into_iter().enumerate().map(
                |(iteration, (process_name, size))| {
                    (
                        process_name,
                        create_vmo_with_some_data(size as usize).read_to_vec(0, size).unwrap(),
                        size,
                        iteration,
                    )
                },
            ),
        );
    }
}
