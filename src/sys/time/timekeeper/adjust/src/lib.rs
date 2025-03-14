// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Serves the `fuchsia.time.external/Adjust` FIDL API.

use anyhow::{Context, Result};
use futures::channel::mpsc;
use futures::{SinkExt, StreamExt};
use log::{debug, error};
use scopeguard::defer;
use std::cell::RefCell;
use {fidl_fuchsia_time_external as ffte, fuchsia_runtime as fxr, fuchsia_trace as trace};

#[derive(Debug, PartialEq)]
pub enum Command {
    /// A power management command.
    PowerManagement,
    /// Report a reference point for the boot-timeline-to-utc-timeline affine
    /// transform.
    Reference { boot_reference: zx::BootInstant, utc_reference: fxr::UtcInstant },
}

/// Serves the "Adjust" FIDL API.
pub struct Server {
    // Every Adjust input is forwarded to this sender.
    adjust_sender: RefCell<mpsc::Sender<Command>>,
}

impl Server {
    /// Creates a new [Server].
    ///
    /// The `adjust_sender` channel must always have enough room to accepts a new adjustment
    /// without blocking.
    pub fn new(adjust_sender: mpsc::Sender<Command>) -> Self {
        // RefCell to avoid &mut self where not essential.
        Self { adjust_sender: RefCell::new(adjust_sender) }
    }

    pub async fn serve(&self, mut stream: ffte::AdjustRequestStream) -> Result<()> {
        debug!("time_adjust::serve: entering serving loop");
        defer! {
            debug!("time_adjust::serve: exited  serving loop");
        };
        while let Some(request) = stream.next().await {
            trace::duration!(c"timekeeper", c"adjust:request");
            debug!("time_adjust::Server::serve: request: {:?}", request);
            match request {
                Ok(ffte::AdjustRequest::ReportBootToUtcMapping {
                    boot_reference,
                    utc_reference,
                    responder,
                }) => {
                    trace::instant!(c"alarms", c"adjust:request:params", trace::Scope::Process,
                        "boot_reference" => boot_reference.into_nanos(), "utc_reference" => utc_reference);
                    let utc_reference = fxr::UtcInstant::from_nanos(utc_reference);
                    let command = Command::Reference { boot_reference, utc_reference };
                    self.adjust_sender
                        .borrow_mut()
                        .send(command)
                        .await
                        .context("while trying to send to adjust_sender")?;
                    responder.send(Ok(())).context("while trying to respond to a FIDL request")?;
                }
                Err(e) => {
                    error!("FIDL error: {:?}", e);
                }
            };
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fuchsia_async as fasync;

    #[fuchsia::test]
    async fn basic_test() -> Result<()> {
        let (tx, mut rx) = mpsc::channel(1);
        let (proxy, stream) = fidl::endpoints::create_proxy_and_stream::<ffte::AdjustMarker>();
        let server = Server::new(tx);
        let _task = fasync::Task::local(async move { server.serve(stream).await });

        let _success = proxy
            .report_boot_to_utc_mapping(zx::BootInstant::from_nanos(42), 4200i64)
            .await
            .expect("infallible");
        let recv = rx.next().await.expect("infallible");

        assert_eq!(
            Command::Reference {
                boot_reference: zx::BootInstant::from_nanos(42),
                utc_reference: fxr::UtcInstant::from_nanos(4200)
            },
            recv
        );

        Ok(())
    }
}
