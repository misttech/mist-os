// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_inspect_selfprofile_test::{PuppetRequest, PuppetRequestStream};
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::component::inspector;
use fuchsia_inspect_contrib::{
    profile_duration, start_self_profiling, stop_self_profiling, ProfileDuration,
};
use futures::StreamExt;

#[fuchsia::main]
async fn main() {
    // Set up inspect and serving.
    let mut fs = ServiceFs::new();
    let _inspect_server =
        inspect_runtime::publish(inspector(), inspect_runtime::PublishOptions::default());
    fs.dir("svc").add_fidl_service(|requests: PuppetRequestStream| requests);
    fs.take_and_serve_directory_handle().unwrap();

    // Listen for requests to run profiled code.
    while let Some(mut requests) = fs.next().await {
        while let Some(Ok(request)) = requests.next().await {
            match request {
                PuppetRequest::StartProfiling { responder } => {
                    inspector().root().record_lazy_child(
                        "this_name_doesnt_matter",
                        ProfileDuration::lazy_node_callback,
                    );
                    start_self_profiling();
                    responder.send().unwrap();
                }
                PuppetRequest::StopProfiling { responder } => {
                    stop_self_profiling();
                    responder.send().unwrap();
                }
                PuppetRequest::RunProfiledFunction { responder } => {
                    profile_duration!("RootDuration");
                    for _ in 0..10 {
                        // Durations in a loop can accumulate error.
                        profile_duration!("FirstNestedDuration");
                        burn_a_little_cpu();
                    }
                    {
                        profile_duration!("SecondNestedDuration");
                        burn_a_little_cpu();
                        burn_a_little_cpu();
                        burn_a_little_cpu();
                    }
                    {
                        let mut guard = ProfileDuration::enter("ThirdNestedDuration");
                        burn_a_little_cpu();
                        burn_a_little_cpu();
                        guard.pivot("FourthNestedDuration");
                        burn_a_little_cpu();
                    }
                    responder.send().unwrap();
                }
            }
        }
    }
}

fn burn_a_little_cpu() {
    profile_duration!("LeafDuration");
    let mut s = String::new();
    for _ in 0..100_000 {
        s.push('a');
    }

    // Make sure LLVM doesn't get too clever and prevent us from accumulating CPU time by
    // optimizing out the string creation.
    std::hint::black_box(s);
}
