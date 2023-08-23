// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This modules contains most of the top-level logic of the sampler
//! service. It defines functions to process a stream of profiling
//! requests and produce a complete profile, as well as utilities to
//! persist it.

use crate::{crash_reporter::ProfileReport, pprof::pproto, profile_builder::ProfileBuilder};

use anyhow::{Context, Error};
use fidl_fuchsia_memory_sampler::{
    SamplerRequest, SamplerRequestStream, SamplerSetProcessInfoRequest,
};
use fuchsia_async::Task;
use fuchsia_component::server::ServiceFs;
use futures::{channel::mpsc, prelude::*, TryFutureExt};

const MAX_CONCURRENT_REQUESTS: usize = 10_000;

/// Accumulate profiling information in the builder.
async fn process_sampler_request(
    builder: &mut ProfileBuilder,
    request: SamplerRequest,
) -> Result<&mut ProfileBuilder, Error> {
    match request {
        SamplerRequest::RecordAllocation { address, stack_trace, size, .. } => {
            builder.allocate(address, stack_trace, size);
        }
        SamplerRequest::RecordDeallocation { address, stack_trace, .. } => {
            builder.deallocate(address, stack_trace);
        }
        SamplerRequest::SetProcessInfo { payload, .. } => {
            let SamplerSetProcessInfoRequest { process_name, module_map, .. } = payload;
            builder.set_process_info(process_name, module_map.into_iter().flatten());
        }
    };
    Ok(builder)
}

/// Build a profile from a stream of profiling requests. Requests are
/// processed sequentially, in order.
async fn process_sampler_requests(
    stream: impl Stream<Item = Result<SamplerRequest, fidl::Error>>,
) -> Result<(String, pproto::Profile), Error> {
    let mut profile_builder = ProfileBuilder::default();
    stream
        .map(|request| request.context("failed request"))
        .try_fold(&mut profile_builder, process_sampler_request)
        .await?;
    Ok(profile_builder.build())
}

/// Serves the `Sampler` protocol for a given process. Once a client
/// closes their connection, enqueues a `pprof`-compatible profile to
/// the provided channel, for further processing.
///
/// Note: this function does not retry pushing profiles through the
/// channel. Failure to handle profile reports in a timely manner will
/// cause this component to shut down.
///
/// TODO(fxbug.dev/132410): Capture periodic snapshots while the
/// instrumented process is running.
async fn run_sampler_service(
    stream: SamplerRequestStream,
    mut tx: mpsc::Sender<ProfileReport>,
) -> Result<(), Error> {
    let (process_name, profile) = process_sampler_requests(stream).await?;
    tx.try_send(ProfileReport { process_name, profile })?;
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
    Ok(Task::spawn(
        service_fs
            .map(Ok)
            .try_for_each_concurrent(
                MAX_CONCURRENT_REQUESTS,
                move |IncomingServiceRequest::Sampler(stream)| {
                    run_sampler_service(stream, tx.clone())
                },
            )
            .inspect_err(|e| tracing::error!("fuchsia.memory.sampler/Sampler protocol: {}", e)),
    ))
}

#[cfg(test)]
mod test {
    use anyhow::Error;
    use fidl::endpoints::create_proxy_and_stream;
    use fidl_fuchsia_memory_sampler::SamplerSetProcessInfoRequest;
    use fidl_fuchsia_memory_sampler::{ExecutableSegment, ModuleMap, SamplerMarker, StackTrace};
    use itertools::assert_equal;
    use itertools::sorted;

    use crate::sampler_service::{
        pproto::{Location, Mapping},
        process_sampler_requests,
    };

    #[fuchsia::test]
    async fn test_process_sampler_requests_full_profile() -> Result<(), Error> {
        let (client, request_stream) = create_proxy_and_stream::<SamplerMarker>()?;
        let profile_future = process_sampler_requests(request_stream);

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

        let (process_name, profile) = profile_future.await?;

        assert_eq!("test process", process_name);
        assert_eq!(3, profile.mapping.len());
        assert_eq!(4, profile.location.len());
        assert_eq!(3, profile.sample.len());

        Ok(())
    }

    #[fuchsia::test]
    async fn test_process_sampler_requests_set_process_info() -> Result<(), Error> {
        let (client, request_stream) = create_proxy_and_stream::<SamplerMarker>()?;
        let profile_future = process_sampler_requests(request_stream);

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

        let (process_name, profile) = profile_future.await?;
        assert_eq!("test process", process_name);
        assert_eq!(3, profile.mapping.len());
        assert_eq!(Vec::<Location>::new(), profile.location);

        Ok(())
    }

    #[fuchsia::test]
    async fn test_process_sampler_requests_multiple_set_process_info() -> Result<(), Error> {
        let (client, request_stream) = create_proxy_and_stream::<SamplerMarker>()?;
        let profile_future = process_sampler_requests(request_stream);

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

        let (process_name, profile) = profile_future.await?;
        assert_eq!("other test process", process_name);
        assert_eq!(6, profile.mapping.len());
        assert_eq!(Vec::<Location>::new(), profile.location);

        Ok(())
    }

    #[fuchsia::test]
    async fn test_process_sampler_requests_allocate() -> Result<(), Error> {
        let (client, request_stream) = create_proxy_and_stream::<SamplerMarker>()?;
        let profile_future = process_sampler_requests(request_stream);

        let allocation_stack_trace =
            StackTrace { stack_frames: Some(vec![1000, 1500]), ..Default::default() };

        client.record_allocation(0x100, &allocation_stack_trace, 100)?;
        drop(client);

        let (process_name, profile) = profile_future.await?;
        assert_eq!(String::default(), process_name);
        let locations = profile.location.into_iter().map(|Location { address, .. }| address);
        assert_equal(vec![1000, 1500].into_iter(), sorted(locations));

        Ok(())
    }

    #[fuchsia::test]
    async fn test_process_sampler_requests_deallocate() -> Result<(), Error> {
        let (client, request_stream) = create_proxy_and_stream::<SamplerMarker>()?;
        let profile_future = process_sampler_requests(request_stream);

        let stack_trace = StackTrace { stack_frames: Some(vec![3000, 3001]), ..Default::default() };

        client.record_deallocation(0x100, &stack_trace)?;
        drop(client);

        let (process_name, profile) = profile_future.await?;

        assert_eq!(String::default(), process_name);
        assert_eq!(Vec::<Mapping>::new(), profile.mapping);
        assert_eq!(Vec::<Location>::new(), profile.location);

        Ok(())
    }
}
