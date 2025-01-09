// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use anyhow::{Context, Error};
use argh::FromArgs;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::health::Reporter;
use fuchsia_inspect::{self as inspect};
use futures::{StreamExt, TryStreamExt};
use log::{info, warn};
use sampler_component_config::Config as ComponentConfig;
use {
    fidl_fuchsia_hardware_power_statecontrol as reboot, fuchsia_async as fasync,
    sampler_config as config,
};

mod diagnostics;
mod executor;

/// The name of the subcommand and the logs-tag.
pub const PROGRAM_NAME: &str = "sampler";

/// Arguments used to configure sampler.
#[derive(Debug, Default, FromArgs, PartialEq)]
#[argh(subcommand, name = "sampler")]
pub struct Args {}

pub async fn main() -> Result<(), Error> {
    // Serve inspect.
    let inspector = inspect::component::inspector();
    let _inspect_server_task =
        inspect_runtime::publish(inspector, inspect_runtime::PublishOptions::default());

    let mut service_fs = ServiceFs::new();
    service_fs.take_and_serve_directory_handle()?;
    fasync::Task::spawn(async move {
        service_fs.collect::<()>().await;
    })
    .detach();

    // Starting service.
    inspect::component::health().set_starting_up();

    let component_config = ComponentConfig::take_from_startup_handle();
    inspector
        .root()
        .record_child("config", |config_node| component_config.record_inspect(config_node));

    let sampler_config = format!("{}/metrics", component_config.configs_path);
    let fire_config = format!("{}/fire", component_config.configs_path);

    match config::SamplerConfigBuilder::default()
        .minimum_sample_rate_sec(component_config.minimum_sample_rate_sec)
        .sampler_dir(sampler_config)
        .fire_dir(fire_config)
        .load()
    {
        Ok(sampler_config) => {
            // Create endpoint for the reboot watcher register.
            let (reboot_watcher_client, reboot_watcher_request_stream) =
                fidl::endpoints::create_request_stream::<reboot::RebootWatcherMarker>();

            {
                // Let the transient connection fall out of scope once we've passed the client
                // end to our callback server.
                let reboot_watcher_register =
                    connect_to_protocol::<reboot::RebootMethodsWatcherRegisterMarker>()
                        .context("Connect to Reboot watcher register")?;

                reboot_watcher_register
                    .register_watcher(reboot_watcher_client)
                    .await
                    .context("Providing the reboot register with callback channel.")?;
            }

            info!("publishing inspect");
            sampler_config.publish_inspect(inspector.root());

            let sampler_executor = executor::SamplerExecutor::new(sampler_config.clone()).await?;

            // Trigger the project samplers and returns a TaskCancellation struct used to trigger
            // reboot shutdown of sampler.
            let task_canceller = sampler_executor.execute();

            inspect::component::health().set_ok();
            reboot_watcher(reboot_watcher_request_stream, task_canceller).await;

            // Keep the original config around until termination.
            // Otherwise we cannot inspect its value.
            let _sampler_config = sampler_config;
            Ok(())
        }
        Err(e) => {
            warn!(
                "Failed to parse sampler configurations from /config/data/(metrics|fire): {:?}",
                e
            );
            Ok(())
        }
    }
}

async fn reboot_watcher(
    mut stream: reboot::RebootWatcherRequestStream,
    task_canceller: executor::TaskCancellation,
) {
    if let Some(reboot::RebootWatcherRequest::OnReboot { options: _, responder }) =
        stream.try_next().await.unwrap_or_else(|err| {
            // If the channel closed for some reason, we can just let Sampler keep running
            // until component manager kills it.
            warn!("Reboot callback channel closed: {:?}", err);
            None
        })
    {
        task_canceller.perform_reboot_cleanup().await;

        // acknowledge reboot notification to unblock before timeout.
        responder
            .send()
            .unwrap_or_else(|err| warn!("Acking the reboot register failed: {:?}", err));

        info!("Sampler has been halted due to reboot. Goodbye.");
    } else {
        // The reboot watcher channel somehow died. There's no reason to
        // clean ourselves up early, might as well just run until the component
        // manager tells us to stop or all tasks finish.
        task_canceller.run_without_cancellation().await;
        info!("All Sampler tasks have finished running. Goodbye.");
    }
}
