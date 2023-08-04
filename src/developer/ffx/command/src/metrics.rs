// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use analytics::{get_notice, opt_out_for_this_invocation};
use ffx_config::EnvironmentContext;
use ffx_metrics::{add_ffx_launch_and_timing_events, init_metrics_svc};
use fuchsia_async::TimeoutExt;
use itertools::Itertools;
use std::{
    io::Write,
    time::{Duration, Instant},
};

use crate::{FfxContext, Result};

const UNKNOWN_SDK: &str = "Unknown SDK";
pub struct MetricsSession {
    enabled: bool,
    session_start: Instant,
}

pub struct CommandStats {
    pub success: bool,
    pub command_duration: Duration,
    pub analytics_duration: Option<Duration>,
}

impl std::fmt::Display for CommandStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self { success, command_duration, analytics_duration } = self;
        write!(f, "success: {success}, command time: {}, ", command_duration.as_secs_f32())?;
        match analytics_duration {
            Some(analytics_duration) => {
                write!(f, "analytics time: {}", analytics_duration.as_secs_f32())
            }
            None => write!(f, "analytics disabled"),
        }
    }
}

impl MetricsSession {
    pub async fn start(context: &EnvironmentContext) -> Result<Self> {
        let invoker = context.get("fuchsia.analytics.ffx_invoker").await.unwrap_or(None);
        let build_info = context.build_info();
        let enabled = context.analytics_enabled().await;
        let sdk_version;
        if enabled {
            sdk_version =
                get_sdk_version(&context).await.unwrap_or_else(|| UNKNOWN_SDK.to_string());
        } else {
            sdk_version = UNKNOWN_SDK.to_string();
        }
        init_metrics_svc(build_info, invoker.clone(), sdk_version).await;
        if !enabled {
            opt_out_for_this_invocation().await?
        }
        let session_start = Instant::now();
        Ok(Self { enabled, session_start })
    }

    pub async fn print_notice(&self, out: &mut impl Write) -> Result<()> {
        if let Some(note) = get_notice().await {
            writeln!(out, "{}", note).bug()?;
        }
        Ok(())
    }

    pub async fn command_finished(
        self,
        success: bool,
        sanitized_args: &[impl AsRef<str>],
    ) -> Result<CommandStats> {
        let command_done = Instant::now();
        let command_duration = command_done - self.session_start;
        let analytics_duration = if self.enabled {
            tracing::debug!("Command completed. Success: {}", success);
            let timing_in_millis = command_duration.as_millis().to_string();
            let sanitized_args = sanitized_args.iter().map(AsRef::as_ref).join(" ");

            let analytics_task = fuchsia_async::Task::local(async move {
                if let Err(e) =
                    add_ffx_launch_and_timing_events(sanitized_args, timing_in_millis).await
                {
                    tracing::error!("metrics submission failed: {}", e);
                }
                Instant::now()
            });

            let analytics_done = analytics_task
                // TODO(66918): make configurable, and evaluate chosen time value.
                .on_timeout(Duration::from_secs(2), || {
                    tracing::error!("metrics submission timed out");
                    // Metrics timeouts should not impact user flows.
                    Instant::now()
                })
                .await;
            let analytics_duration = analytics_done - command_done;

            Some(analytics_duration)
        } else {
            None
        };
        let stats = CommandStats { success, command_duration, analytics_duration };
        tracing::info!("Command finished. {stats}",);
        Ok(stats)
    }
}

async fn get_sdk_version(context: &EnvironmentContext) -> Option<String> {
    match context.get_sdk().await {
        Ok(sdk) => sdk.get_version_string(),
        Err(_) => None,
    }
}
