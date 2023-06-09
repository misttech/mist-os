// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The Archivist collects and stores diagnostic data from components.

#![warn(clippy::all)]
#![warn(missing_docs)]

use anyhow::{Context, Error};
use archivist_config::Config;
use archivist_lib::{archivist::Archivist, component_lifecycle, events::router::RouterOptions};
use diagnostics_log::PublishOptions;
use fuchsia_async as fasync;
use fuchsia_component::server::{MissingStartupHandle, ServiceFs};
use fuchsia_inspect::{component, health::Reporter};
use fuchsia_zircon as zx;
use tracing::{debug, info, warn, Level, Subscriber};
use tracing_subscriber::{
    fmt::{
        format::{self, FormatEvent, FormatFields},
        FmtContext,
    },
    registry::LookupSpan,
};

const INSPECTOR_SIZE: usize = 2 * 1024 * 1024 /* 2MB */;

fn main() -> Result<(), Error> {
    let config = Config::take_from_startup_handle();
    let num_threads = config.num_threads;
    debug!("Running executor with {} threads.", num_threads);
    if num_threads == 1 {
        let mut executor = fasync::LocalExecutor::new();
        executor.run_singlethreaded(async_main(config)).context("async main")?;
    } else {
        let mut executor = fasync::SendExecutor::new(num_threads as usize - 1);
        executor.run(async_main(config)).context("async main")?;
    }
    debug!("Exiting.");
    Ok(())
}

async fn async_main(config: Config) -> Result<(), Error> {
    init_diagnostics(&config).await.context("initializing diagnostics")?;
    component::inspector()
        .root()
        .record_child("config", |config_node| config.record_inspect(config_node));

    let router_options = RouterOptions {
        validate: config.enable_event_source || config.enable_component_event_provider,
    };
    let mut archivist = Archivist::new(config).await;
    archivist.set_lifecycle_request_stream(component_lifecycle::take_lifecycle_request_stream());
    debug!("Archivist initialized from configuration.");

    let startup_handle =
        fuchsia_runtime::take_startup_handle(fuchsia_runtime::HandleType::DirectoryRequest.into())
            .ok_or(MissingStartupHandle)?;

    let mut fs = ServiceFs::new();
    fs.serve_connection(fidl::endpoints::ServerEnd::new(zx::Channel::from(startup_handle)))?;
    archivist.run(fs, router_options).await?;

    Ok(())
}

async fn init_diagnostics(config: &Config) -> Result<(), Error> {
    if config.log_to_debuglog {
        stdout_to_debuglog::init().await.unwrap();
        tracing_subscriber::fmt()
            .event_format(DebugLogEventFormatter)
            .with_writer(std::io::stdout)
            .with_max_level(Level::INFO)
            .init();
    } else {
        diagnostics_log::initialize(PublishOptions::default().tags(&["embedded"]))?;
    }

    if config.log_to_debuglog {
        info!("Logging started.");
    }

    component::init_inspector_with_size(INSPECTOR_SIZE);
    component::health().set_starting_up();

    fuchsia_trace_provider::trace_provider_create_with_fdio();
    Ok(())
}

struct DebugLogEventFormatter;

impl<S, N> FormatEvent<S, N> for DebugLogEventFormatter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self,
        ctx: &FmtContext<'_, S, N>,
        mut writer: format::Writer<'_>,
        event: &tracing::Event<'_>,
    ) -> std::fmt::Result {
        let level = *event.metadata().level();
        write!(writer, "[archivist] {level}: ")?;
        ctx.field_format().format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}
