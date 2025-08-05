// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{handle_recording_error, TraceData};
use anyhow::Result;
use errors::ffx_bail;
use ffx_config::EnvironmentContext;
use ffx_target::get_target_specifier;
use fidl_fuchsia_developer_ffx::{TargetQuery, TraceOptions, TracingProxy};
use fidl_fuchsia_tracing_controller::TraceConfig;

pub(crate) async fn trace(
    context: &EnvironmentContext,
    proxy: TracingProxy,
    output: String,
    options: TraceOptions,
    trace_config: TraceConfig,
) -> Result<()> {
    let target_spec: Option<String> = get_target_specifier(&context).await?;
    let default = TargetQuery { string_matcher: target_spec, ..Default::default() };

    let res = proxy.start_recording(&default, &output, &options, &trace_config).await?;
    if let Err(e) = res {
        ffx_bail!("{}", handle_recording_error(&context, e, &output).await);
    }
    Ok(())
}

pub(crate) async fn stop_tracing(
    context: &EnvironmentContext,
    proxy: TracingProxy,
    output: String,
) -> Result<TraceData> {
    let res = proxy.stop_recording(&output).await?;
    match res {
        Ok((_target, output_file, categories, stop_result)) => {
            Ok(TraceData { output_file, categories, stop_result })
        }
        Err(e) => ffx_bail!("{}", handle_recording_error(context, e, &output).await),
    }
}
