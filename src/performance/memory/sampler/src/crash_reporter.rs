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
use fuchsia_zircon::Duration;
use futures::{channel::mpsc, stream::once, StreamExt, TryFutureExt, TryStreamExt};
use itertools::Itertools;

use crate::pprof::pproto;
use crate::profile_builder::profile_to_vmo;

const CRASH_PRODUCT_NAME: &str = "FuchsiaHeapProfile";
const CRASH_PROGRAM_NAME: &str = "memory_sampler";
const CRASH_SIGNATURE: &str = "fuchsia-memory-profile";
const MAX_CONCURRENT_PROFILES: usize = 10;
const MAX_SNAPSHOT_RATE_PER_HOUR: i64 = 1;

#[derive(PartialEq, Debug)]
pub struct ProfileReport {
    pub process_name: String,
    pub profile: pproto::Profile,
}

/// File a crash report with the provided profiles attached to it.
///
/// Note: Care should be taken to not call this function too often, as
/// to not overload the crash reporting system. As a safe baseline,
/// aim to never file more than a crash report per hour.
///
/// Note: profiles, when attached, are grouped by process name, then
/// numbered. E.g. if there are two profiles for "process 1" and one
/// profile for "process 2", then they will be named "process 1_0",
/// "process 1_1" and "process 2_0".
///
/// Note: panics if called with more than
/// `MAX_NUM_ATTACHMENTS_PER_CRASH_REPORT` profiles.
async fn file_report(
    profiles: &Vec<ProfileReport>,
    crash_reporter: &CrashReporterProxy,
) -> Result<(), Error> {
    assert!(profiles.len() <= MAX_NUM_ATTACHMENTS_PER_CRASH_REPORT as usize);
    if profiles.is_empty() {
        return Ok(());
    }
    let attachments: Result<Vec<Attachment>, Error> = profiles
        .iter()
        .group_by(|p| &p.process_name)
        .into_iter()
        .flat_map(|(_, profiles)| profiles.enumerate())
        .map(|(i, profile_report)| {
            let (vmo, size) = profile_to_vmo(&profile_report.profile)?;
            Ok(Attachment {
                key: format!("{}_{}", profile_report.process_name, i),
                value: Buffer { vmo, size },
            })
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
        .chain(Interval::new(Duration::from_hours(MAX_SNAPSHOT_RATE_PER_HOUR)));
    let (tx, rx) = mpsc::channel::<ProfileReport>(MAX_CONCURRENT_PROFILES);
    let crash_reporter_task = Task::spawn({
        let ready_profiles = rx.ready_chunks(MAX_NUM_ATTACHMENTS_PER_CRASH_REPORT as usize);
        ready_profiles.zip(throttler_stream).map(|(profile, _)| Ok(profile)).try_for_each(
            |profiles| async move {
                let crash_reporter = connect_to_protocol::<CrashReporterMarker>()?;
                file_report(&profiles, &crash_reporter)
                    .inspect_err(|e| tracing::error!("Filing report: {}", e))
                    .await
            },
        )
    });
    (tx, crash_reporter_task)
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::pprof::pproto::{Mapping, Sample};
    use fidl::endpoints::create_proxy_and_stream;
    use fidl_fuchsia_feedback::CrashReporterRequest;
    use futures::{try_join, FutureExt};
    use prost::Message;

    fn handle_crash_reporter_request(request: CrashReporterRequest) -> CrashReport {
        match request {
            CrashReporterRequest::File { report, responder } => {
                let _ = responder.send(Ok(()));
                report
            }
            CrashReporterRequest::FileReport { report, responder } => {
                let _ = responder.send(Ok(&Default::default()));
                report
            }
        }
    }

    fn retrieve_profile_from_attachment(attachment: &Attachment) -> ProfileReport {
        let Attachment { key, value, .. } = attachment;
        let encoded_profile = value.vmo.read_to_vec(0, value.size).unwrap();
        ProfileReport {
            // This function assumes that attachment names are process
            // names with a `_[0-9]*` suffix appended.
            process_name: key.rsplit_once('_').unwrap().0.to_string(),
            profile: pproto::Profile::decode(&encoded_profile[..]).unwrap(),
        }
    }

    #[fuchsia::test]
    async fn test_file_report_no_profile() {
        let (client, _) = create_proxy_and_stream::<CrashReporterMarker>().unwrap();
        let no_profiles = vec![];
        let file_report_future = file_report(&no_profiles, &client);
        file_report_future.now_or_never().unwrap().unwrap()
    }

    #[fuchsia::test]
    async fn test_file_report() {
        let (client, mut request_stream) =
            create_proxy_and_stream::<CrashReporterMarker>().unwrap();

        // Create some profiles with some arbitrary data.
        let profiles = vec![
            // First profile report for a first process, with some
            // samples.
            ProfileReport {
                process_name: "test_process_1".to_string(),
                profile: pproto::Profile {
                    sample: vec![Sample { location_id: vec![1, 2, 3], ..Default::default() }],
                    ..Default::default()
                },
            },
            // First profile report for a second process, with some
            // mapping.
            ProfileReport {
                process_name: "test_process_2".to_string(),
                profile: pproto::Profile {
                    mapping: vec![Mapping { memory_start: 42, ..Default::default() }],
                    ..Default::default()
                },
            },
            // Second profile report for the first process, with some
            // mapping.
            ProfileReport {
                process_name: "test_process_1".to_string(),
                profile: pproto::Profile {
                    mapping: vec![Mapping { memory_start: 42, ..Default::default() }],
                    ..Default::default()
                },
            },
        ];
        let file_report_future = file_report(&profiles, &client);
        let (_, report) = try_join!(
            file_report_future,
            request_stream
                .next()
                .map(|item| Ok(handle_crash_reporter_request(item.unwrap().unwrap())))
        )
        .unwrap();

        assert_eq!(report.program_name, Some(CRASH_PROGRAM_NAME.to_string()));
        assert_eq!(report.crash_signature, Some(CRASH_SIGNATURE.to_string()));
        let attachments: Vec<ProfileReport> = report
            .attachments
            .as_ref()
            .unwrap()
            .iter()
            .map(retrieve_profile_from_attachment)
            .collect();
        assert_eq!(attachments, profiles);
    }
}
