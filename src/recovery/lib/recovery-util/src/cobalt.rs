// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Context, Error};
use fidl_fuchsia_cobalt::{AggregateAndUploadMarker, AggregateAndUploadSynchronousProxy};
use fidl_fuchsia_metrics::{
    MetricEventLoggerFactoryMarker, MetricEventLoggerFactoryProxy, MetricEventLoggerProxy,
    ProjectSpec,
};
use fuchsia_async as fasync;
use fuchsia_component::client::{connect_channel_to_protocol, connect_to_protocol};
use log::error;
#[cfg(test)]
use mockall::automock;

#[cfg_attr(test, automock)]
pub trait Cobalt {
    fn aggregate_and_upload(&self, timeout_seconds: i64) -> Result<(), Error>;
}

#[derive(Default)]
pub struct CobaltImpl;

impl CobaltImpl {
    fn aggregate_and_upload_with_sync_proxy(
        timeout_seconds: i64,
        proxy: AggregateAndUploadSynchronousProxy,
    ) -> Result<(), Error> {
        let deadline =
            zx::MonotonicInstant::after(zx::MonotonicDuration::from_seconds(timeout_seconds));
        proxy
            .aggregate_and_upload_metric_events(deadline)
            .map_err(|e| format_err!("AggregateAndUploadMetric returned an error: {:?}", e))
    }
}

impl Cobalt for CobaltImpl {
    fn aggregate_and_upload(&self, timeout_seconds: i64) -> Result<(), Error> {
        let (server_end, client_end) = zx::Channel::create();
        connect_channel_to_protocol::<AggregateAndUploadMarker>(server_end)
            .context("Failed to connect to the Cobalt AggregateAndUploadMarker")?;
        let cobalt_proxy = AggregateAndUploadSynchronousProxy::new(client_end);

        Self::aggregate_and_upload_with_sync_proxy(timeout_seconds, cobalt_proxy)
    }
}

/// Creates a LoggerProxy connected to Cobalt.
///
/// The connection is performed in a Future run on the global executor, but the `LoggerProxy`
/// can be used immediately.
///
/// This function takes a `MetricEventLoggerFactoryProxy` as argument, so it's testable.
///
/// # Returns
/// `LoggerProxy` for log messages to be sent to.
fn get_logger_from_factory(
    factory_proxy: MetricEventLoggerFactoryProxy,
) -> Result<MetricEventLoggerProxy, Error> {
    let (logger_proxy, server_end) = fidl::endpoints::create_proxy();
    fasync::Task::spawn(async move {
        if let Err(err) = factory_proxy
            .create_metric_event_logger(
                &ProjectSpec { project_id: Some(metrics::PROJECT_ID), ..Default::default() },
                server_end,
            )
            .await
        {
            error!(err:%; "Failed to create Cobalt logger");
        }
    })
    .detach();

    Ok(logger_proxy)
}

/// Creates a LoggerProxy connected to Cobalt.
///
/// The connection is performed in a Future run on the global executor, but the `LoggerProxy`
/// can be used immediately.
///
/// # Returns
/// `LoggerProxy` for log messages to be sent to.
pub fn get_logger() -> Result<MetricEventLoggerProxy, Error> {
    let logger_factory = connect_to_protocol::<MetricEventLoggerFactoryMarker>()
        .context("Failed to connect to the Cobalt MetricEventLoggerFactory")?;

    get_logger_from_factory(logger_factory)
}

/// Reports the duration of the OTA download.
///
/// # Parameters
/// - `logger_proxy`: The cobalt logger.
/// - `duration`: seconds
///
/// # Returns
/// `Ok` if the duration was logged successfully.
pub async fn log_ota_duration(
    logger_proxy: &MetricEventLoggerProxy,
    duration: i64,
) -> Result<(), Error> {
    if duration < 0 {
        return Err(format_err!("duration must not be negative"));
    }

    logger_proxy
        .log_integer(metrics::OTA_DOWNLOAD_DURATION_METRIC_ID, duration, &[])
        .await
        .context("Could not log ota dowload duration.")?
        .map_err(|e| format_err!("Logging ota download duration returned an error: {:?}", e))
}

/// Reports the Recovery stages
///
/// # Parameters
/// - `logger_proxy`: The cobalt logger.
/// - `code`:
///      refer to the metrics.yaml
///
/// # Returns
/// `Ok` if the status was logged successfully.
pub async fn log_recovery_stage(
    logger_proxy: &MetricEventLoggerProxy,
    status: metrics::RecoveryEventMetricDimensionResult,
) -> Result<(), Error> {
    logger_proxy
        .log_occurrence(metrics::RECOVERY_EVENT_METRIC_ID, 1, &[status as u32])
        .await
        .context("Could not log recovery stage event.")?
        .map_err(|e| format_err!("Logging recovery stage event returned an error: {:?}", e))
}

// Call cobalt log functions and checks error code
#[macro_export]
macro_rules! log_metric {
    ($func_name:expr,$arg:expr) => {
        if let Ok(cobalt_logger) = cobalt::get_logger() {
            if let Err(err) = $func_name(&cobalt_logger, $arg).await {
                eprintln!("Failed to log metric ({}): {:?}", stringify!($func_name), err)
            }
        }
    };
}

pub use log_metric;
pub use recovery_metrics_registry::cobalt_registry as metrics;

#[cfg(test)]
mod tests {
    use super::*;
    use fidl::endpoints::create_proxy_and_stream;
    use fidl_fuchsia_metrics::{
        MetricEvent, MetricEventLoggerMarker, MetricEventLoggerRequest, MetricEventPayload,
    };
    use futures::TryStreamExt;
    use mock_metrics::MockMetricEventLoggerFactory;
    use std::sync::Arc;

    /// Tests that the get_logger_from_factory can return the correct logger proxy
    #[fasync::run_singlethreaded(test)]
    async fn test_get_logger_from_factory() {
        let mock = Arc::new(MockMetricEventLoggerFactory::with_id(metrics::PROJECT_ID));
        let (factory, stream) = create_proxy_and_stream::<MetricEventLoggerFactoryMarker>();
        let task = fasync::Task::spawn(mock.clone().run_logger_factory(stream));
        let logger = get_logger_from_factory(factory).unwrap();
        task.await;

        let events = &[MetricEvent {
            metric_id: 42,
            event_codes: vec![],
            payload: MetricEventPayload::Count(1),
        }];
        logger.log_metric_events(events).await.unwrap().unwrap();
        let mock_events = mock.wait_for_at_least_n_events_with_metric_id(1, 42).await;
        assert_eq!(mock_events, events);
    }

    /// Tests that the right payload is sent to Cobalt when logging the ota download time.
    #[fasync::run_singlethreaded(test)]
    async fn test_log_ota_duration() {
        let (logger_proxy, mut logger_server) =
            create_proxy_and_stream::<MetricEventLoggerMarker>();
        let duration = 255;

        fasync::Task::spawn(async move {
            let _ = log_ota_duration(&logger_proxy, duration).await;
        })
        .detach();

        if let Some(log_request) = logger_server.try_next().await.unwrap() {
            if let MetricEventLoggerRequest::LogInteger {
                metric_id,
                value,
                event_codes,
                responder: _,
            } = log_request
            {
                assert_eq!(metric_id, metrics::OTA_DOWNLOAD_DURATION_METRIC_ID);
                assert!(event_codes.is_empty());
                assert_eq!(value, duration);
            } else {
                panic!("LogInteger failed");
            }
        } else {
            panic!("logger_server.try_next failed");
        }
    }

    /// Tests that an error is raised if duration < 0.
    #[fasync::run_singlethreaded(test)]
    async fn test_log_ota_negative_duration() {
        let (logger_proxy, _logger_server) = create_proxy_and_stream::<MetricEventLoggerMarker>();

        let result = log_ota_duration(&logger_proxy, -5).await;
        assert!(result.is_err());
        assert_eq!(format!("{}", result.unwrap_err()), "duration must not be negative");
    }

    /// Tests that the right payload is sent to Cobalt when logging the recovery stages.
    #[fasync::run_singlethreaded(test)]
    async fn test_log_recovery_stage() {
        let (logger_proxy, mut logger_server) =
            create_proxy_and_stream::<MetricEventLoggerMarker>();
        let status = metrics::RecoveryEventMetricDimensionResult::OtaStarted;

        fasync::Task::spawn(async move {
            let _ = log_recovery_stage(&logger_proxy, status).await;
        })
        .detach();

        if let Some(log_request) = logger_server.try_next().await.unwrap() {
            if let MetricEventLoggerRequest::LogOccurrence {
                metric_id,
                count,
                event_codes,
                responder: _,
            } = log_request
            {
                assert_eq!(metric_id, metrics::RECOVERY_EVENT_METRIC_ID);
                assert_eq!(
                    event_codes,
                    &[metrics::RecoveryEventMetricDimensionResult::OtaStarted as u32]
                );
                assert_eq!(count, 1);
            } else {
                panic!("LogOccurance failed");
            }
        } else {
            panic!("logger_server.try_next failed");
        }
    }
}
