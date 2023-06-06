// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_component_runner as frunner;
use fuchsia_component::{client::connect_to_protocol, server::ServiceFs};
use futures::{StreamExt, TryStreamExt};
use kernel_manager::StarnixKernel;

/// The component URL of the Starnix kernel.
const KERNEL_URL: &str = "fuchsia-pkg://fuchsia.com/starnix_kernel#meta/starnix_kernel.cm";

enum Services {
    ComponentRunner(frunner::ComponentRunnerRequestStream),
}

#[fuchsia::main(logging_tags = ["starnix_runner"])]
async fn main() -> Result<(), Error> {
    let mut fs = ServiceFs::new_local();
    fs.dir("svc").add_fidl_service(Services::ComponentRunner);
    fs.take_and_serve_directory_handle()?;
    fs.for_each_concurrent(None, |request: Services| async {
        match request {
            Services::ComponentRunner(stream) => {
                serve_component_runner(stream).await.expect("failed to start component runner")
            }
        }
    })
    .await;
    Ok(())
}

async fn serve_component_runner(
    mut stream: frunner::ComponentRunnerRequestStream,
) -> Result<(), Error> {
    while let Some(event) = stream.try_next().await? {
        match event {
            frunner::ComponentRunnerRequest::Start { start_info, controller, .. } => {
                let realm = connect_to_protocol::<fcomponent::RealmMarker>()
                    .expect("Failed to connect to realm.");
                StarnixKernel::create(realm, KERNEL_URL, start_info, controller).await?;
            }
        }
    }
    Ok(())
}
