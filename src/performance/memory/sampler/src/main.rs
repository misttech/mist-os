// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! # Memory Sampler
//!
//! Memory Sampler is a component that serves the
//! `fuchsia.memory.sampler/Sampler` protocol, used by applications to
//! report their memory usage. It collects allocation information
//! across the lifetime of a process('s connection), and writes
//! `pprof`-compatible profile files once done.

mod pprof;
mod profile_builder;
mod sampler_service;

use anyhow::Error;
use fidl_fuchsia_memory_sampler::SamplerRequestStream;
use fuchsia_component::server::ServiceFs;
use futures::prelude::*;

enum IncomingServiceRequest {
    Sampler(SamplerRequestStream),
}

/// Serve the `fuchsia.memory.sampler/Sampler` protocol.
#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut service_fs = ServiceFs::new_local();
    service_fs.dir("svc").add_fidl_service(IncomingServiceRequest::Sampler);
    service_fs.take_and_serve_directory_handle()?;
    const MAX_CONCURRENT: usize = 10000;
    service_fs
        .for_each_concurrent(MAX_CONCURRENT, |IncomingServiceRequest::Sampler(stream)| {
            sampler_service::run_sampler_service(stream).unwrap_or_else(|e| {
                tracing::error!("fuchsia.memory.sampler/Sampler protocol: {}", e)
            })
        })
        .await;

    Ok(())
}
