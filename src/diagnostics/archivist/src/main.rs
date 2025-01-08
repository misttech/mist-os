// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The Archivist collects and stores diagnostic data from components.

#![warn(missing_docs)]

use anyhow::{Context, Error};
use archivist_config::Config;
use archivist_lib::archivist::Archivist;
use archivist_lib::component_lifecycle;
use diagnostics_log::PublishOptions;
use fuchsia_component::client;
use fuchsia_component::server::{MissingStartupHandle, ServiceFs};
use fuchsia_inspect::component;
use fuchsia_inspect::health::Reporter;
use log::{debug, info, LevelFilter};
use std::fmt::Write;

use {fidl_fuchsia_component_sandbox as fsandbox, fuchsia_async as fasync};

const INSPECTOR_SIZE: usize = 2 * 1024 * 1024 /* 2MB */;

struct GlobalLogger;

struct StringVisitor(String);
impl log::kv::VisitSource<'_> for StringVisitor {
    fn visit_pair(
        &mut self,
        key: log::kv::Key<'_>,
        value: log::kv::Value<'_>,
    ) -> Result<(), log::kv::Error> {
        value.visit(StringValueVisitor { buf: &mut self.0, key: key.as_str() })
    }
}
struct StringValueVisitor<'a> {
    buf: &'a mut String,
    key: &'a str,
}
impl log::kv::VisitValue<'_> for StringValueVisitor<'_> {
    fn visit_any(&mut self, value: log::kv::Value<'_>) -> Result<(), log::kv::Error> {
        write!(self.buf, " {}={}", self.key, value).expect("writing into strings does not fail");
        Ok(())
    }
}

impl log::Log for GlobalLogger {
    fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
        // Always true, because the actual severity
        // is controlled with log::set_max_level
        // and log::max_level.
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        if self.enabled(record.metadata()) {
            let msg_buffer = format!("{}", record.args());
            let mut visitor = StringVisitor(msg_buffer);
            let _ = record.key_values().visit(&mut visitor);
            let msg_buffer = visitor.0;

            let msg_prefix = format!("[archivist] {}: ", record.level());

            // &str pointing to the remains of the message.
            let mut msg = msg_buffer.as_str();
            while !msg.is_empty() {
                // Split the message if it contains a newline or is too long for
                // the debug log.
                let mut split_point = if let Some(newline_pos) = msg.find('\n') {
                    newline_pos + 1
                } else {
                    msg.len()
                };
                split_point =
                    std::cmp::min(split_point, zx::sys::ZX_LOG_RECORD_DATA_MAX - msg_prefix.len());

                // Ensure the split point is at a character boundary - splitting
                // in the middle of a unicode character causes a panic.
                while !msg.is_char_boundary(split_point) {
                    split_point -= 1;
                }

                let msg_to_write = format!("{}{}\n", msg_prefix, &msg[..split_point]);

                std::io::Write::write_all(&mut std::io::stdout(), msg_to_write.as_bytes()).unwrap();
                msg = &msg[split_point..];
            }
        }
    }

    fn flush(&self) {}
}

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
    let component_store = client::connect_to_protocol::<fsandbox::CapabilityStoreMarker>()
        .context("connect to factory")?;
    let lifecycle_requests = component_lifecycle::take_lifecycle_request_stream();
    archivist.run(fs, is_embedded, component_store, lifecycle_requests).await?;

    Ok(())
}

async fn init_diagnostics(config: &Config) -> Result<(), Error> {
    if config.log_to_debuglog {
        stdout_to_debuglog::init().await.unwrap();
        log::set_max_level(LevelFilter::Info);
        log::set_logger(&GlobalLogger).unwrap();
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
