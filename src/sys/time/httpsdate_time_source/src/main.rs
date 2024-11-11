// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod bound;
mod constants;
mod datatypes;
mod diagnostics;
mod httpsdate;
mod sampler;

use crate::diagnostics::{
    CobaltDiagnostics, CompositeDiagnostics, Diagnostics, InspectDiagnostics,
};
use crate::httpsdate::{HttpsDateUpdateAlgorithm, RetryStrategy};
use crate::sampler::{HttpsSampler, HttpsSamplerImpl};
use anyhow::{Context, Error};
use fidl_fuchsia_net_interfaces::StateMarker;
use fidl_fuchsia_time_external::{
    PullSourceRequestStream, PushSourceRequestStream, Status, Urgency,
};
use fuchsia_component::server::{ServiceFs, ServiceObj};

use futures::future::{join, Future};
use futures::{FutureExt, StreamExt};
use pull_source::PullSource;
use push_source::PushSource;
use std::collections::HashMap;
use tracing::warn;

/// Retry strategy used while polling for time.
const RETRY_STRATEGY: RetryStrategy = RetryStrategy {
    // This being on a boot timeline means suspension will cause timeouts.
    min_between_failures: zx::BootDuration::from_seconds(1),
    max_exponent: 3,
    tries_per_exponent: 3,
    converge_time_between_samples: zx::BootDuration::from_minutes(2),
    maintain_time_between_samples: zx::BootDuration::from_minutes(20),
};

/// HttpsDate config, populated from build-time generated structured config.
pub struct Config {
    // The amount of time to wait for a HTTPs timeout for sampling.
    // Looks like it could be boot duration, to ensure that requests fail fast
    // when sampling is much delayed.
    https_timeout: zx::BootDuration,
    standard_deviation_bound_percentage: u8,
    first_rtt_time_factor: u16,
    use_pull_api: bool,
    sample_config_by_urgency: HashMap<Urgency, SampleConfig>,
}

pub struct SampleConfig {
    max_attempts: u32,
    num_polls: u32,
}

impl From<httpsdate_config::Config> for Config {
    fn from(source: httpsdate_config::Config) -> Self {
        let sample_config_by_urgency = [
            (
                Urgency::Low,
                SampleConfig {
                    max_attempts: source.max_attempts_urgency_low,
                    num_polls: source.num_polls_urgency_low,
                },
            ),
            (
                Urgency::Medium,
                SampleConfig {
                    max_attempts: source.max_attempts_urgency_medium,
                    num_polls: source.num_polls_urgency_medium,
                },
            ),
            (
                Urgency::High,
                SampleConfig {
                    max_attempts: source.max_attempts_urgency_high,
                    num_polls: source.num_polls_urgency_high,
                },
            ),
        ]
        .into_iter()
        .collect();
        Config {
            https_timeout: zx::BootDuration::from_seconds(source.https_timeout_sec.into()),
            standard_deviation_bound_percentage: source.standard_deviation_bound_percentage,
            first_rtt_time_factor: source.first_rtt_time_factor,
            use_pull_api: source.use_pull_api,
            sample_config_by_urgency,
        }
    }
}

/// Serves `PushSource` FIDL API.
pub struct PushServer<
    'a,
    S: HttpsSampler + Send + Sync,
    D: Diagnostics,
    N: Future<Output = Result<(), Error>> + Send,
> {
    push_source: PushSource<HttpsDateUpdateAlgorithm<'a, S, D, N>>,
}

impl<'a, S, D, N> PushServer<'a, S, D, N>
where
    S: HttpsSampler + Send + Sync,
    D: Diagnostics,
    N: Future<Output = Result<(), Error>> + Send,
{
    fn new(
        diagnostics: D,
        sampler: S,
        internet_reachable: N,
        config: &'a Config,
    ) -> Result<Self, Error> {
        let update_algorithm = HttpsDateUpdateAlgorithm::new(
            RETRY_STRATEGY,
            diagnostics,
            sampler,
            internet_reachable,
            config,
        );
        let push_source = PushSource::new(update_algorithm, Status::Initializing)?;

        Ok(PushServer { push_source })
    }

    /// Start serving `PushSource` FIDL API.
    fn serve<'b>(
        &'b self,
        fs: &'b mut ServiceFs<ServiceObj<'static, PushSourceRequestStream>>,
    ) -> Result<impl 'b + Future<Output = Result<(), anyhow::Error>>, Error> {
        let update_fut = self.push_source.poll_updates();

        fs.dir("svc").add_fidl_service(|stream: PushSourceRequestStream| stream);
        let service_fut = fs.for_each_concurrent(None, |stream| {
            handle_push_source_request(stream, &self.push_source)
        });
        Ok(join(update_fut, service_fut).map(|(update_result, _serve_result)| update_result))
    }
}

/// Handle next `PushSource` FIDL API request.
async fn handle_push_source_request<T: push_source::UpdateAlgorithm>(
    stream: PushSourceRequestStream,
    push_source: &PushSource<T>,
) {
    push_source
        .handle_requests_for_stream(stream)
        .await
        .unwrap_or_else(|e| warn!("Error handling PushSource stream: {:?}", e));
}

/// Serves `PullSource` FIDL API.
pub struct PullServer<
    'a,
    S: HttpsSampler + Send + Sync,
    D: Diagnostics,
    N: Future<Output = Result<(), Error>> + Send,
> {
    pull_source: PullSource<HttpsDateUpdateAlgorithm<'a, S, D, N>>,
}

impl<'a, S, D, N> PullServer<'a, S, D, N>
where
    S: HttpsSampler + Send + Sync,
    D: Diagnostics,
    N: Future<Output = Result<(), Error>> + Send,
{
    fn new(
        diagnostics: D,
        sampler: S,
        internet_reachable: N,
        config: &'a Config,
    ) -> Result<Self, Error> {
        let update_algorithm = HttpsDateUpdateAlgorithm::new(
            RETRY_STRATEGY,
            diagnostics,
            sampler,
            internet_reachable,
            config,
        );
        let pull_source = PullSource::new(update_algorithm)?;

        Ok(PullServer { pull_source })
    }

    /// Start serving `PullSource` FIDL API.
    fn serve<'b>(
        &'b self,
        fs: &'b mut ServiceFs<ServiceObj<'static, PullSourceRequestStream>>,
    ) -> Result<impl 'b + Future<Output = Result<(), anyhow::Error>>, Error> {
        fs.dir("svc").add_fidl_service(|stream: PullSourceRequestStream| stream);
        Ok(fs
            .for_each_concurrent(None, |stream| {
                handle_pull_source_request(stream, &self.pull_source)
            })
            .map(|_| Ok(())))
    }
}

/// Handle next `PullSource` FIDL API request.
async fn handle_pull_source_request<T: pull_source::UpdateAlgorithm>(
    stream: PullSourceRequestStream,
    pull_source: &PullSource<T>,
) {
    pull_source
        .handle_requests_for_stream(stream)
        .await
        .unwrap_or_else(|e| warn!("Error handling PullSource stream: {:?}", e));
}

/// Serves FIDL interfaces provided by the component.
async fn serve<S, D, N>(
    config: &'_ Config,
    sampler: S,
    diagnostics: D,
    internet_reachable: N,
) -> Result<(), Error>
where
    S: HttpsSampler + Send + Sync,
    D: Diagnostics,
    N: Future<Output = Result<(), Error>> + Send,
{
    let _inspect_server_task = inspect_runtime::publish(
        fuchsia_inspect::component::inspector(),
        inspect_runtime::PublishOptions::default(),
    );

    if config.use_pull_api {
        let mut fs = ServiceFs::new();

        fs.take_and_serve_directory_handle()?;

        let server = PullServer::new(diagnostics, sampler, internet_reachable, config)?;
        let result = server.serve(&mut fs)?.await;
        result
    } else {
        let mut fs = ServiceFs::new();

        fs.take_and_serve_directory_handle()?;

        let server = PushServer::new(diagnostics, sampler, internet_reachable, config)?;
        let result = server.serve(&mut fs)?.await;
        result
    }
}

#[fuchsia::main(logging_tags=["time", "source"])]
async fn main() -> Result<(), Error> {
    let config = httpsdate_config::Config::take_from_startup_handle();
    let time_source_url = config.time_source_endpoint_url.clone();

    let inspector = fuchsia_inspect::component::inspector();
    // Export structured configuration into diagnostics.
    inspector.root().record_child("config", |config_node| config.record_inspect(config_node));
    let inspect = InspectDiagnostics::new(inspector.root());

    // From here on, use the local type `Config` for configuration bits.
    let config: Config = config.into();

    let (cobalt, cobalt_sender_fut) = CobaltDiagnostics::new();
    let diagnostics = CompositeDiagnostics::new(inspect, cobalt);

    let sampler = HttpsSamplerImpl::new(time_source_url.parse()?, &config);

    let interface_state_service = fuchsia_component::client::connect_to_protocol::<StateMarker>()
        .context("failed to connect to fuchsia.net.interfaces/State")?;
    let internet_reachable = fidl_fuchsia_net_interfaces_ext::wait_for_reachability(
        fidl_fuchsia_net_interfaces_ext::event_stream_from_state(
            &interface_state_service,
            fidl_fuchsia_net_interfaces_ext::IncludedAddresses::OnlyAssigned,
        )
        .context("failed to create network interface event stream")?,
    )
    .map(|r| r.context("reachability status stream error"));

    let serve_fut = serve(&config, sampler, diagnostics, internet_reachable);

    let (update_res, _) = join(serve_fut, cobalt_sender_fut).await;
    update_res
}
