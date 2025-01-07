// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This modules contains most of the top-level logic of the sampler
//! service. It defines functions to process a stream of profiling
//! requests and produce a complete profile, as well as utilities to
//! persist it.
use crate::crash_reporter::ProfileReport;
use crate::profile_builder::ProfileBuilder;

use anyhow::{Context, Error};
use fidl_fuchsia_memory_sampler::{
    SamplerRequest, SamplerRequestStream, SamplerSetProcessInfoRequest,
};
use fuchsia_async::Task;
use fuchsia_component::server::ServiceFs;
use futures::channel::mpsc;
use futures::prelude::*;
use std::time::{Duration, Instant};

/// The threshold of recorded stack traces to trigger a partial
/// report. The overhead for each stack trace is of the order of 1
/// KiB; keeping this below 1000 should keep the residual memory
/// *for a single profiled process* roughly under ~1 MiB.
const RECLAIMABLE_STACK_TRACES_PROFILE_THRESHOLD: usize = 1000;

/// Upper bound on the number of concurrent connections served.
const MAX_CONCURRENT_REQUESTS: usize = 10;

/// Upper bound on the elapsed time between producing two partial
/// profiles.
const MAX_DURATION_BETWEEN_PARTIAL_PROFILES: Duration = Duration::from_secs(12 * 60 * 60);

/// Accumulate profiling information in the builder. May send a
/// partial profile depending on the amount of recorded data, at least
/// once every `MAX_DURATION_BETWEEN_PARTIAL_PROFILES`.
async fn process_sampler_request<'a>(
    builder: &'a mut ProfileBuilder,
    tx: &'a mut mpsc::Sender<ProfileReport>,
    request: SamplerRequest,
    index: usize,
    mut time_of_last_profile: Instant,
) -> Result<(&'a mut ProfileBuilder, &'a mut mpsc::Sender<ProfileReport>, Instant), Error> {
    match request {
        SamplerRequest::RecordAllocation { address, stack_trace, size, .. } => {
            builder.allocate(address, stack_trace.stack_frames.unwrap_or_default(), size);
        }
        SamplerRequest::RecordDeallocation { address, stack_trace, .. } => {
            builder.deallocate(address, stack_trace.stack_frames.unwrap_or_default());
        }
        SamplerRequest::SetProcessInfo { payload, .. } => {
            let SamplerSetProcessInfoRequest { process_name, module_map, .. } = payload;
            builder.set_process_info(process_name, module_map.into_iter().flatten());
        }
    };

    // File a partial profile under one of two conditions:
    //
    // * The recorded data reached a size threshold, and it's time to
    //   file a profile to reclaim some memory.
    //
    // * MAX_DURATION_BETWEEN_PARTIAL_PROFILES has elapsed since the
    //   last time we filed a partial profile for this process.
    let now = Instant::now();
    if (now - time_of_last_profile >= MAX_DURATION_BETWEEN_PARTIAL_PROFILES)
        || (builder.get_approximate_reclaimable_stack_traces_count()
            >= RECLAIMABLE_STACK_TRACES_PROFILE_THRESHOLD)
    {
        let profile = builder.build_partial_profile(index)?;
        tx.send(profile).await?;
        time_of_last_profile = now;
    }
    Ok((builder, tx, time_of_last_profile))
}

/// Build a profile from a stream of profiling requests. Requests are
/// processed sequentially, in order.
async fn process_sampler_requests(
    stream: impl Stream<Item = Result<SamplerRequest, fidl::Error>>,
    tx: &mut mpsc::Sender<ProfileReport>,
) -> Result<ProfileReport, Error> {
    let mut profile_builder = ProfileBuilder::default();
    stream
        .enumerate()
        .map(|(i, request)| request.context("failed request").map(|r| (i, r)))
        .try_fold(
            (&mut profile_builder, tx, Instant::now()),
            |(builder, tx, time_of_last_profile), (index, request)| {
                process_sampler_request(builder, tx, request, index, time_of_last_profile)
            },
        )
        .await?;
    profile_builder.build()
}

/// Serves the `Sampler` protocol for a given process. Once a client
/// closes their connection, enqueues a `pprof`-compatible profile to
/// the provided channel, for further processing. May also regularly
/// enqueue partial profiles, to offload the profiler's memory usage.
///
/// Note: this function does not retry pushing profiles through the
/// channel. Failure to handle profile reports in a timely manner will
/// cause this component to shut down.
async fn run_sampler_service(
    stream: SamplerRequestStream,
    mut tx: mpsc::Sender<ProfileReport>,
) -> Result<(), Error> {
    let profile = process_sampler_requests(stream, &mut tx).await?;
    log::debug!("Profiling for {} done, queuing final report", profile.get_process_name());
    tx.send(profile).await?;
    Ok(())
}

enum IncomingServiceRequest {
    Sampler(SamplerRequestStream),
}

/// Returns a task that serves the `fuchsia.memory.sampler/Sampler`
/// protocol.
///
/// Note: any error will cause the sampler service to shutdown.
pub fn setup_sampler_service(
    tx: mpsc::Sender<ProfileReport>,
) -> Result<Task<Result<(), Error>>, Error> {
    let mut service_fs = ServiceFs::new();
    service_fs.dir("svc").add_fidl_service(IncomingServiceRequest::Sampler);
    service_fs.take_and_serve_directory_handle()?;
    Ok(Task::local(
        service_fs
            .map(Ok)
            .try_for_each_concurrent(
                MAX_CONCURRENT_REQUESTS,
                move |IncomingServiceRequest::Sampler(stream)| {
                    run_sampler_service(stream, tx.clone())
                },
            )
            .inspect_err(|e| log::error!("fuchsia.memory.sampler/Sampler protocol: {}", e)),
    ))
}

#[cfg(test)]
mod test {
    use anyhow::Error;
    use fidl::endpoints::{create_proxy_and_stream, RequestStream};
    use fidl_fuchsia_memory_sampler::{
        ExecutableSegment, ModuleMap, SamplerMarker, SamplerRequest, SamplerSetProcessInfoRequest,
        StackTrace,
    };
    use futures::channel::mpsc;
    use futures::{join, StreamExt};
    use itertools::{assert_equal, sorted};
    use prost::Message;
    use std::time::Instant;
    use zx::Vmo;

    use crate::crash_reporter::ProfileReport;
    use crate::pprof::pproto::{Location, Mapping, Profile};
    use crate::sampler_service::{
        process_sampler_request, process_sampler_requests, ProfileBuilder,
        MAX_DURATION_BETWEEN_PARTIAL_PROFILES, RECLAIMABLE_STACK_TRACES_PROFILE_THRESHOLD,
    };

    fn deserialize_profile(profile: Vmo, size: u64) -> Profile {
        Profile::decode(&profile.read_to_vec(0, size).unwrap()[..]).unwrap()
    }

    #[fuchsia::test]
    async fn test_process_sampler_requests_full_profile() -> Result<(), Error> {
        let (client, request_stream) = create_proxy_and_stream::<SamplerMarker>();
        let (mut tx, _rx) = mpsc::channel(1);
        let profile_future = process_sampler_requests(request_stream, &mut tx);

        let module_map = vec![
            ModuleMap {
                build_id: Some(vec![1, 2, 3, 4]),
                executable_segments: Some(vec![
                    ExecutableSegment {
                        start_address: Some(2000),
                        relative_address: Some(0),
                        size: Some(1000),
                        ..Default::default()
                    },
                    ExecutableSegment {
                        start_address: Some(4000),
                        relative_address: Some(2000),
                        size: Some(2000),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            },
            ModuleMap {
                build_id: Some(vec![3, 4, 5, 6]),
                executable_segments: Some(vec![ExecutableSegment {
                    start_address: Some(3000),
                    relative_address: Some(1000),
                    size: Some(100),
                    ..Default::default()
                }]),
                ..Default::default()
            },
        ];
        let allocation_stack_trace =
            StackTrace { stack_frames: Some(vec![1000, 1500]), ..Default::default() };
        let deallocation_stack_trace =
            StackTrace { stack_frames: Some(vec![3000, 3001]), ..Default::default() };

        client.set_process_info(&SamplerSetProcessInfoRequest {
            process_name: Some("test process".to_string()),
            module_map: Some(module_map),
            ..Default::default()
        })?;
        client.record_allocation(0x100, &allocation_stack_trace, 100)?;
        client.record_allocation(0x200, &allocation_stack_trace, 1000)?;
        client.record_deallocation(0x100, &deallocation_stack_trace)?;
        drop(client);

        if let ProfileReport::Final { process_name, size, profile } = profile_future.await? {
            assert_eq!("test process", process_name);
            let profile = deserialize_profile(profile, size);
            assert_eq!(3, profile.mapping.len());
            assert_eq!(4, profile.location.len());
            assert_eq!(3, profile.sample.len());
        } else {
            panic!("Expected complete report, got partial report instead.");
        };

        Ok(())
    }

    #[fuchsia::test]
    async fn test_process_sampler_request_partial_profile_on_size() -> Result<(), Error> {
        let (_client, request_stream) = create_proxy_and_stream::<SamplerMarker>();
        let (mut tx, mut rx) = mpsc::channel(1);
        const TEST_NAME: &str = "test process";
        let mut builder = ProfileBuilder::default();

        // Pre-fill `builder` with a large number of unique dead
        // allocations, to trigger a partial profile on the next
        // request.
        builder.set_process_info(Some(TEST_NAME.to_string()), vec![].into_iter());
        {
            (0..RECLAIMABLE_STACK_TRACES_PROFILE_THRESHOLD as u64).for_each(|i| {
                builder.allocate(i, (i..i + 4).collect(), 10);
                builder.deallocate(i, (i..i + 4).collect());
            });
        }

        let stack_trace = StackTrace { stack_frames: Some(vec![1000, 1500]), ..Default::default() };
        const TEST_INDEX: usize = 42;
        let profile_future = process_sampler_request(
            &mut builder,
            &mut tx,
            SamplerRequest::RecordAllocation {
                address: RECLAIMABLE_STACK_TRACES_PROFILE_THRESHOLD as u64,
                stack_trace,
                size: 10,
                control_handle: request_stream.control_handle(),
            },
            TEST_INDEX,
            Instant::now(),
        );
        let (_, report) = join!(profile_future, rx.next());
        let report = report.unwrap();
        match report {
            ProfileReport::Partial { process_name, iteration, profile, size } => {
                assert_eq!(process_name, TEST_NAME.to_string());
                assert_eq!(iteration, TEST_INDEX);
                // This test assumes that every single allocation ends
                // up as a sample in the produced profile.
                let profile = deserialize_profile(profile, size);
                assert!(profile.sample.len() > RECLAIMABLE_STACK_TRACES_PROFILE_THRESHOLD);
            }
            _ => assert!(false),
        };
        Ok(())
    }

    #[fuchsia::test]
    async fn test_process_sampler_request_partial_profile_on_time() -> Result<(), Error> {
        let (_client, request_stream) = create_proxy_and_stream::<SamplerMarker>();
        let (mut tx, mut rx) = mpsc::channel(1);
        const TEST_NAME: &str = "test process";
        let mut builder = ProfileBuilder::default();
        builder.set_process_info(Some(TEST_NAME.to_string()), vec![].into_iter());

        let stack_trace = StackTrace { stack_frames: Some(vec![1000, 1500]), ..Default::default() };
        const TEST_INDEX: usize = 42;
        let profile_future = process_sampler_request(
            &mut builder,
            &mut tx,
            SamplerRequest::RecordAllocation {
                address: 1,
                stack_trace,
                size: 10,
                control_handle: request_stream.control_handle(),
            },
            TEST_INDEX,
            Instant::now() - MAX_DURATION_BETWEEN_PARTIAL_PROFILES,
        );
        let (_, report) = join!(profile_future, rx.next());
        let report = report.unwrap();
        match report {
            ProfileReport::Partial { process_name, iteration, profile, size } => {
                assert_eq!(process_name, TEST_NAME.to_string());
                assert_eq!(iteration, TEST_INDEX);
                // This test assumes that every single allocation ends
                // up as a sample in the produced profile.
                let profile = deserialize_profile(profile, size);
                assert_eq!(1, profile.sample.len());
            }
            _ => assert!(false),
        };
        Ok(())
    }

    #[fuchsia::test]
    async fn test_process_sampler_requests_set_process_info() -> Result<(), Error> {
        let (client, request_stream) = create_proxy_and_stream::<SamplerMarker>();
        let (mut tx, _rx) = mpsc::channel(1);
        let profile_future = process_sampler_requests(request_stream, &mut tx);

        let module_map = vec![
            ModuleMap {
                build_id: Some(vec![1, 2, 3, 4]),
                executable_segments: Some(vec![
                    ExecutableSegment {
                        start_address: Some(2000),
                        relative_address: Some(0),
                        size: Some(1000),
                        ..Default::default()
                    },
                    ExecutableSegment {
                        start_address: Some(4000),
                        relative_address: Some(2000),
                        size: Some(2000),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            },
            ModuleMap {
                build_id: Some(vec![3, 4, 5, 6]),
                executable_segments: Some(vec![ExecutableSegment {
                    start_address: Some(3000),
                    relative_address: Some(1000),
                    size: Some(100),
                    ..Default::default()
                }]),
                ..Default::default()
            },
        ];

        client.set_process_info(&SamplerSetProcessInfoRequest {
            process_name: Some("test process".to_string()),
            module_map: Some(module_map),
            ..Default::default()
        })?;
        drop(client);

        if let ProfileReport::Final { process_name, profile, size } = profile_future.await? {
            assert_eq!("test process", process_name);
            let profile = deserialize_profile(profile, size);
            assert_eq!(3, profile.mapping.len());
            assert_eq!(Vec::<Location>::new(), profile.location);
        } else {
            panic!("Expected complete report, got partial report instead.");
        };
        Ok(())
    }

    #[fuchsia::test]
    async fn test_process_sampler_requests_multiple_set_process_info() -> Result<(), Error> {
        let (client, request_stream) = create_proxy_and_stream::<SamplerMarker>();
        let (mut tx, _rx) = mpsc::channel(1);
        let profile_future = process_sampler_requests(request_stream, &mut tx);

        let module_map = vec![
            ModuleMap {
                build_id: Some(vec![1, 2, 3, 4]),
                executable_segments: Some(vec![
                    ExecutableSegment {
                        start_address: Some(2000),
                        relative_address: Some(0),
                        size: Some(1000),
                        ..Default::default()
                    },
                    ExecutableSegment {
                        start_address: Some(4000),
                        relative_address: Some(2000),
                        size: Some(2000),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            },
            ModuleMap {
                build_id: Some(vec![3, 4, 5, 6]),
                executable_segments: Some(vec![ExecutableSegment {
                    start_address: Some(3000),
                    relative_address: Some(1000),
                    size: Some(100),
                    ..Default::default()
                }]),
                ..Default::default()
            },
        ];

        client.set_process_info(&SamplerSetProcessInfoRequest {
            process_name: Some("test process".to_string()),
            module_map: Some(module_map.clone()),
            ..Default::default()
        })?;
        client.set_process_info(&SamplerSetProcessInfoRequest {
            process_name: Some("other test process".to_string()),
            module_map: Some(module_map),
            ..Default::default()
        })?;
        drop(client);

        if let ProfileReport::Final { process_name, profile, size } = profile_future.await? {
            assert_eq!("other test process", process_name);
            let profile = deserialize_profile(profile, size);
            assert_eq!(6, profile.mapping.len());
            assert_eq!(Vec::<Location>::new(), profile.location);
        } else {
            panic!("Expected complete report, got partial report instead.");
        };

        Ok(())
    }

    #[fuchsia::test]
    async fn test_process_sampler_requests_allocate() -> Result<(), Error> {
        let (client, request_stream) = create_proxy_and_stream::<SamplerMarker>();
        let (mut tx, _rx) = mpsc::channel(1);
        let profile_future = process_sampler_requests(request_stream, &mut tx);

        let allocation_stack_trace =
            StackTrace { stack_frames: Some(vec![1000, 1500]), ..Default::default() };

        client.record_allocation(0x100, &allocation_stack_trace, 100)?;
        drop(client);

        if let ProfileReport::Final { process_name, profile, size } = profile_future.await? {
            assert_eq!(String::default(), process_name);
            let profile = deserialize_profile(profile, size);
            let locations = profile.location.into_iter().map(|Location { address, .. }| address);
            assert_equal(vec![1000, 1500].into_iter(), sorted(locations));
        } else {
            panic!("Expected complete report, got partial report instead.");
        };
        Ok(())
    }

    #[fuchsia::test]
    async fn test_process_sampler_requests_deallocate() -> Result<(), Error> {
        let (client, request_stream) = create_proxy_and_stream::<SamplerMarker>();
        let (mut tx, _rx) = mpsc::channel(1);
        let profile_future = process_sampler_requests(request_stream, &mut tx);

        let stack_trace = StackTrace { stack_frames: Some(vec![3000, 3001]), ..Default::default() };

        client.record_deallocation(0x100, &stack_trace)?;
        drop(client);

        if let ProfileReport::Final { process_name, profile, size } = profile_future.await? {
            assert_eq!(String::default(), process_name);
            let profile = deserialize_profile(profile, size);
            assert_eq!(Vec::<Mapping>::new(), profile.mapping);
            assert_eq!(Vec::<Location>::new(), profile.location);
        } else {
            panic!("Expected complete report, got partial report instead.");
        };

        Ok(())
    }
}
