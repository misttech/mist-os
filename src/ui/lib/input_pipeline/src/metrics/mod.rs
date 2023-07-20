// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context as _, Error};
use cobalt_client::traits::AsEventCode;
use fidl_fuchsia_metrics as metrics;
use fuchsia_async as fasync;
use metrics_registry::*;
use tracing::warn;

/// Connects to the MetricEventLoggerFactory service to create a
/// MetricEventLoggerProxy for the caller.
fn create_metrics_logger() -> Result<metrics::MetricEventLoggerProxy, Error> {
    let factory_proxy =
        fuchsia_component::client::connect_to_protocol::<metrics::MetricEventLoggerFactoryMarker>()
            .context("connecting to metrics")?;

    let (cobalt_proxy, cobalt_server) =
        fidl::endpoints::create_proxy::<metrics::MetricEventLoggerMarker>()
            .context("creating MetricEventLoggerProxy")?;

    let project_spec = metrics::ProjectSpec {
        customer_id: None, // defaults to fuchsia
        project_id: Some(PROJECT_ID),
        ..Default::default()
    };

    fasync::Task::spawn(async move {
        match factory_proxy.create_metric_event_logger(&project_spec, cobalt_server).await {
            Err(e) => warn!("FIDL failure setting up event logger: {e:?}"),
            Ok(Err(e)) => warn!("CreateMetricEventLogger failure: {e:?}"),
            Ok(Ok(())) => {}
        }
    })
    .detach();

    Ok(cobalt_proxy)
}

fn log_on_failure(result: Result<Result<(), metrics::Error>, fidl::Error>) {
    match result {
        Ok(Ok(())) => (),
        e => warn!("failed to log metrics: {:?}", e),
    };
}

/// A client connection to the Cobalt logging service.
#[derive(Clone, Default)]
pub struct MetricsLogger(Option<metrics::MetricEventLoggerProxy>);

impl MetricsLogger {
    pub fn new() -> Self {
        let logger =
            create_metrics_logger().map_err(|e| warn!("Failed to create metrics logger: {e}")).ok();
        Self(logger)
    }

    /// Logs an error occurrence metric using the Cobalt logger. Does not block execution.
    pub fn log_error<E: AsEventCode, S: Into<String>>(&self, event_code: E, message: S) {
        tracing::error!("{}", message.into());

        let Some(c) = self.0.clone() else { return };
        let code = event_code.as_event_code();
        fuchsia_async::Task::spawn(async move {
            log_on_failure(c.log_occurrence(INPUT_PIPELINE_ERROR_METRIC_ID, 1, &[code]).await);
        })
        .detach();
    }
}
