// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Result};
use async_trait::async_trait;
use errors::ffx_bail;
use fho::{moniker, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_power_metrics::{self as fmetrics, CpuLoad, Metric};
use {
    ffx_cpu_load_args as args_mod, fidl_fuchsia_developer_remotecontrol as rc,
    fidl_fuchsia_kernel as fstats,
};

#[derive(FfxTool)]
pub struct CpuLoadTool {
    #[command]
    cmd: args_mod::CpuLoadCommand,
    rcs_proxy: rc::RemoteControlProxy,
    #[with(moniker("/core/metrics-logger"))]
    cpu_logger: fmetrics::RecorderProxy,
}

fho::embedded_plugin!(CpuLoadTool);

#[async_trait(?Send)]
impl FfxMain for CpuLoadTool {
    type Writer = SimpleWriter;
    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        let CpuLoadTool { cmd, rcs_proxy, cpu_logger } = self;
        match (cmd.subcommand, cmd.duration) {
            (Some(subcommand), None) => match subcommand {
                args_mod::SubCommand::Start(start_cmd) => start(cpu_logger, start_cmd).await?,
                args_mod::SubCommand::Stop(_) => stop(cpu_logger).await?,
            },
            (None, Some(duration)) => {
                let stats_proxy =
                    match rcs::kernel_stats(&rcs_proxy, std::time::Duration::from_secs(5)).await {
                        Ok(s) => s,
                        Err(e) => ffx_bail!("Could not open fuchsia.kernel.Stats: {e}",),
                    };

                measure(stats_proxy, duration, &mut writer).await?;
            }
            _ => ffx_bail!(
                "Please specify a duration for immediate load display, or alternatively, utilize \
            the start/stop subcommand to instruct the metrics-logger component to record the \
            CPU usage data on the target."
            ),
        }
        Ok(())
    }
}

pub async fn measure<W: std::io::Write>(
    stats_proxy: fstats::StatsProxy,
    duration: std::time::Duration,
    writer: &mut W,
) -> Result<()> {
    if duration.is_zero() {
        bail!("Duration must be > 0");
    }
    let cpu_loads = stats_proxy.get_cpu_load(duration.as_nanos() as i64).await?;
    print_loads(cpu_loads, writer)?;

    Ok(())
}

/// Prints a vector of CPU load values in the following format:
///     CPU 0: 0.66%
///     CPU 1: 1.56%
///     CPU 2: 0.83%
///     CPU 3: 0.71%
///     Total: 3.76%
fn print_loads<W: std::io::Write>(cpu_load_pcts: Vec<f32>, writer: &mut W) -> Result<()> {
    for (i, load_pct) in cpu_load_pcts.iter().enumerate() {
        writeln!(writer, "CPU {}: {:.2}%", i, load_pct)?;
    }

    writeln!(writer, "Total: {:.2}%", cpu_load_pcts.iter().sum::<f32>())?;
    Ok(())
}

pub async fn start(cpu_logger: fmetrics::RecorderProxy, cmd: args_mod::StartCommand) -> Result<()> {
    let interval_ms = cmd.interval.as_millis() as u32;

    // Dispatch to Recorder.StartLogging or Recorder.StartLoggingForever,
    // depending on whether a logging duration is specified.
    let result = if let Some(duration) = cmd.duration {
        let duration_ms = duration.as_millis() as u32;
        cpu_logger
            .start_logging(
                "ffx_cpu",
                &[Metric::CpuLoad(CpuLoad { interval_ms })],
                duration_ms,
                cmd.output_to_syslog,
                false,
            )
            .await?
    } else {
        cpu_logger
            .start_logging_forever(
                "ffx_cpu",
                &[Metric::CpuLoad(CpuLoad { interval_ms })],
                cmd.output_to_syslog,
                false,
            )
            .await?
    };

    match result {
        Err(fmetrics::RecorderError::InvalidSamplingInterval) => ffx_bail!(
            "Recorder.StartLogging received an invalid sampling interval. \n\
            Please check if `interval` meets the following requirements: \n\
            1) Must be smaller than `duration` if `duration` is specified; \n\
            2) Must not be smaller than 500ms if `output_to_syslog` is enabled."
        ),
        Err(fmetrics::RecorderError::AlreadyLogging) => ffx_bail!(
            "Ffx cpu load is already active. Use \"stop\" subcommand to stop the active \
            loggingg manually."
        ),
        Err(fmetrics::RecorderError::TooManyActiveClients) => ffx_bail!(
            "Recorder is running too many clients. Retry after any other client is stopped."
        ),
        Err(fmetrics::RecorderError::Internal) => {
            ffx_bail!("Request failed due to an internal error. Check syslog for more details.")
        }
        _ => Ok(()),
    }
}

pub async fn stop(cpu_logger: fmetrics::RecorderProxy) -> Result<()> {
    if !cpu_logger.stop_logging("ffx_cpu").await? {
        ffx_bail!("Stop logging returned false; Check if logging is already inactive.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use fidl_fuchsia_power_metrics::{self as fmetrics};
    use futures::channel::mpsc;
    use std::time::Duration;

    // Create a metrics-logger that expects a specific request type (Start, StartForever, or
    // Stop), and returns a specific error
    macro_rules! make_proxy {
        ($request_type:tt, $error_type:tt) => {
            fho::testing::fake_proxy(move |req| match req {
                fmetrics::RecorderRequest::$request_type { responder, .. } => {
                    responder.send(Err(fmetrics::RecorderError::$error_type)).unwrap();
                }
                _ => {
                    panic!("Expected RecorderRequest::{}; got {:?}", stringify!($request_type), req)
                }
            })
        };
    }

    const ONE_SEC: Duration = Duration::from_secs(1);

    /// Tests that invalid arguments are rejected.
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_invalid_args() {
        let (proxy, _) = fidl::endpoints::create_proxy::<fstats::StatsMarker>();
        assert!(measure(proxy, Duration::from_secs(0), &mut std::io::stdout()).await.is_err());
    }

    /// Tests that the input parameter for duration is correctly converted between seconds and
    /// nanoseconds. The test uses a duration parameter of one second.
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_cpu_load_duration() {
        let (duration_request_sender, mut duration_request_receiver) = mpsc::channel(1);

        let proxy = fidl::endpoints::spawn_stream_handler(move |req| {
            let mut duration_request_sender = duration_request_sender.clone();
            async move {
                match req {
                    fstats::StatsRequest::GetCpuLoad { duration, responder } => {
                        duration_request_sender.try_send(duration).unwrap();
                        let _ = responder.send(&[]); // returned values don't matter for this test
                    }
                    request => panic!("Unexpected request: {:?}", request),
                }
            }
        })
        .unwrap();

        let _ = measure(proxy, Duration::from_secs(1), &mut std::io::stdout()).await.unwrap();

        match duration_request_receiver.try_next() {
            Ok(Some(duration_request)) => {
                assert_eq!(duration_request as u128, Duration::from_secs(1).as_nanos())
            }
            e => panic!("Failed to get duration_request: {:?}", e),
        }
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_cpu_load_output() {
        let proxy = fidl::endpoints::spawn_stream_handler(move |req| async move {
            let data = vec![0.66f32, 1.56, 0.83, 0.71];
            match req {
                fstats::StatsRequest::GetCpuLoad { responder, .. } => {
                    let _ = responder.send(&data.clone());
                }
                request => panic!("Unexpected request: {:?}", request),
            }
        })
        .unwrap();

        let mut writer = Vec::new();
        let _ = measure(proxy, Duration::from_secs(1), &mut writer).await.unwrap();

        let output = String::from_utf8(writer).expect("valid utf8 output");
        assert_eq!(
            output,
            "\
CPU 0: 0.66%
CPU 1: 1.56%
CPU 2: 0.83%
CPU 3: 0.71%
Total: 3.76%
",
        );
    }

    /// Confirms that the start logging request is dispatched to FIDL requests as expected.
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_request_dispatch_start_logging() {
        // Start logging: interval=1s, duration=4s
        let args = args_mod::StartCommand {
            interval: ONE_SEC,
            duration: Some(4 * ONE_SEC),
            output_to_syslog: false,
        };
        let (mut sender, mut receiver) = mpsc::channel(1);
        let proxy = fho::testing::fake_proxy(move |req| match req {
            fmetrics::RecorderRequest::StartLogging {
                client_id,
                metrics,
                duration_ms,
                output_samples_to_syslog,
                output_stats_to_syslog,
                responder,
            } => {
                assert_eq!(String::from("ffx_cpu"), client_id);
                assert_eq!(metrics.len(), 1);
                assert_eq!(metrics[0], Metric::CpuLoad(CpuLoad { interval_ms: 1000 }),);
                assert_eq!(output_samples_to_syslog, false);
                assert_eq!(output_stats_to_syslog, false);
                assert_eq!(duration_ms, 4000);
                responder.send(Ok(())).unwrap();
                sender.try_send(()).unwrap();
            }
            _ => panic!("Expected RecorderRequest::StartLogging; got {:?}", req),
        });
        start(proxy, args).await.unwrap();
        assert_matches!(receiver.try_next().unwrap(), Some(()));
    }

    /// Confirms that the start logging forever request is dispatched to FIDL requests as expected.
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_request_dispatch_start_logging_forever() {
        // Start logging: interval=1s, duration=forever
        let args =
            args_mod::StartCommand { interval: ONE_SEC, duration: None, output_to_syslog: false };
        let (mut sender, mut receiver) = mpsc::channel(1);
        let proxy = fho::testing::fake_proxy(move |req| match req {
            fmetrics::RecorderRequest::StartLoggingForever {
                client_id,
                metrics,
                output_samples_to_syslog,
                output_stats_to_syslog,
                responder,
                ..
            } => {
                assert_eq!(String::from("ffx_cpu"), client_id);
                assert_eq!(metrics.len(), 1);
                assert_eq!(metrics[0], Metric::CpuLoad(CpuLoad { interval_ms: 1000 }),);
                assert_eq!(output_samples_to_syslog, false);
                assert_eq!(output_stats_to_syslog, false);
                responder.send(Ok(())).unwrap();
                sender.try_send(()).unwrap();
            }
            _ => panic!("Expected RecorderRequest::StartLoggingForever; got {:?}", req),
        });
        start(proxy, args).await.unwrap();
        assert_matches!(receiver.try_next().unwrap(), Some(()));
    }

    /// Confirms that the stop logging request is dispatched to FIDL requests as expected.
    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_request_dispatch_stop_logging() {
        // Stop logging
        let (mut sender, mut receiver) = mpsc::channel(1);
        let proxy = fho::testing::fake_proxy(move |req| match req {
            fmetrics::RecorderRequest::StopLogging { client_id, responder } => {
                assert_eq!(String::from("ffx_cpu"), client_id);
                responder.send(true).unwrap();
                sender.try_send(()).unwrap();
            }
            _ => panic!("Expected RecorderRequest::StopLogging; got {:?}", req),
        });
        stop(proxy).await.unwrap();
        assert_matches!(receiver.try_next().unwrap(), Some(()));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_stop_logging_error() {
        let proxy = fho::testing::fake_proxy(move |req| match req {
            fmetrics::RecorderRequest::StopLogging { responder, .. } => {
                responder.send(false).unwrap();
            }
            _ => panic!("Expected RecorderRequest::StopLogging; got {:?}", req),
        });
        let error = stop(proxy).await.unwrap_err();
        assert!(error.to_string().contains("Stop logging returned false"));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_start_logging_interval_error() {
        let args = args_mod::StartCommand {
            interval: ONE_SEC,
            duration: Some(2 * ONE_SEC),
            output_to_syslog: false,
        };
        let proxy = make_proxy!(StartLogging, InvalidSamplingInterval);
        let error = start(proxy, args).await.unwrap_err();
        assert!(error.to_string().contains("invalid sampling interval"));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_start_logging_forever_interval_error() {
        let args =
            args_mod::StartCommand { interval: ONE_SEC, duration: None, output_to_syslog: false };
        let proxy = make_proxy!(StartLoggingForever, InvalidSamplingInterval);
        let error = start(proxy, args).await.unwrap_err();
        assert!(error.to_string().contains("invalid sampling interval"));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_start_logging_already_active_error() {
        let args = args_mod::StartCommand {
            interval: ONE_SEC,
            duration: Some(2 * ONE_SEC),
            output_to_syslog: false,
        };
        let proxy = make_proxy!(StartLogging, AlreadyLogging);
        let error = start(proxy, args).await.unwrap_err();
        assert!(error.to_string().contains("already active"));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_start_logging_forever_already_active_error() {
        let args =
            args_mod::StartCommand { interval: ONE_SEC, duration: None, output_to_syslog: false };
        let proxy = make_proxy!(StartLoggingForever, AlreadyLogging);
        let error = start(proxy, args).await.unwrap_err();
        assert!(error.to_string().contains("already active"));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_start_logging_too_many_clients_error() {
        let args = args_mod::StartCommand {
            interval: ONE_SEC,
            duration: Some(2 * ONE_SEC),
            output_to_syslog: false,
        };
        let proxy = make_proxy!(StartLogging, TooManyActiveClients);
        let error = start(proxy, args).await.unwrap_err();
        assert!(error.to_string().contains("too many clients"));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_start_logging_forever_too_many_clients_error() {
        let args =
            args_mod::StartCommand { interval: ONE_SEC, duration: None, output_to_syslog: false };
        let proxy = make_proxy!(StartLoggingForever, TooManyActiveClients);
        let error = start(proxy, args).await.unwrap_err();
        assert!(error.to_string().contains("too many clients"));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_start_logging_internal_error() {
        let args = args_mod::StartCommand {
            interval: ONE_SEC,
            duration: Some(2 * ONE_SEC),
            output_to_syslog: false,
        };
        let proxy = make_proxy!(StartLogging, Internal);
        let error = start(proxy, args).await.unwrap_err();
        assert!(error.to_string().contains("an internal error"));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_start_logging_forever_internal_error() {
        let args =
            args_mod::StartCommand { interval: ONE_SEC, duration: None, output_to_syslog: false };
        let proxy = make_proxy!(StartLoggingForever, Internal);
        let error = start(proxy, args).await.unwrap_err();
        assert!(error.to_string().contains("an internal error"));
    }
}
