// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The Archivist collects and stores diagnostic data from components.

#![warn(missing_docs)]

use anyhow::{Context, Error};
use archivist_config::Config;
use archivist_lib::archivist::Archivist;
use archivist_lib::component_lifecycle;
use archivist_lib::severity_filter::KlogSeverityFilter;
use diagnostics_log::PublishOptions;
use fuchsia_async as fasync;
use fuchsia_component::server::{MissingStartupHandle, ServiceFs};
use fuchsia_inspect::component;
use fuchsia_inspect::health::Reporter;
use tracing::{debug, info, warn, Level, Subscriber};
use tracing_subscriber::fmt::format::{self, FormatEvent, FormatFields};
use tracing_subscriber::fmt::FmtContext;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;

const INSPECTOR_SIZE: usize = 2 * 1024 * 1024 /* 2MB */;

fn main() -> Result<(), Error> {
    let config = Config::take_from_startup_handle();
    // The executor will spin up an extra thread which is only for monitoring, so we ignore that.
    // Non-legacy sockets are serviced from a dedicated thread (which isn't counted here).  We
    // *must* always use a multi-threaded executor because we use `fasync::EHandle::poll_tasks`
    // which requires it.
    let mut executor = fasync::SendExecutor::new(config.num_threads as usize);
    executor.run(async_main(config)).context("async main")?;
    debug!("Exiting.");
    Ok(())
}

async fn async_main(config: Config) -> Result<(), Error> {
    init_diagnostics(&config).await.context("initializing diagnostics")?;
    component::inspector()
        .root()
        .record_child("config", |config_node| config.record_inspect(config_node));

    let is_embedded = !config.log_to_debuglog;
    let archivist = Archivist::new(config).await;
    debug!("Archivist initialized from configuration.");

    let startup_handle =
        fuchsia_runtime::take_startup_handle(fuchsia_runtime::HandleType::DirectoryRequest.into())
            .ok_or(MissingStartupHandle)?;

    let mut fs = ServiceFs::new();
    fs.serve_connection(fidl::endpoints::ServerEnd::new(zx::Channel::from(startup_handle)))?;
    let lifecycle_requests = component_lifecycle::take_lifecycle_request_stream();
    archivist.run(fs, is_embedded, lifecycle_requests).await?;

    Ok(())
}

async fn init_diagnostics(config: &Config) -> Result<(), Error> {
    if config.log_to_debuglog {
        stdout_to_debuglog::init().await.unwrap();
        // NOTE: with_max_level(Level::TRACE) doesn't actually
        // set the log severity to TRACE level, because our filter overrides that.
        // It's needed so that tracing passes our filter all messages so that we can
        // dynamically filter them at runtime.
        tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_ansi(false)
                .event_format(DebugLogEventFormatter)
                .with_writer(std::io::stdout)
                .with_max_level(Level::TRACE)
                .finish()
                .with(KlogSeverityFilter::default()),
        )?;
    } else {
        diagnostics_log::initialize(PublishOptions::default().tags(&["embedded"]))?;
    }

    if config.log_to_debuglog {
        info!("archivist: started.");
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
