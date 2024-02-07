// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
mod env_info;
mod ga4_event;
mod ga4_metrics_service;
mod metrics_state;
mod notice;

use {
    anyhow::{bail, Result},
    futures::lock::Mutex,
    std::collections::BTreeMap,
    std::path::PathBuf,
    std::sync::Arc,
    std::sync::OnceLock,
};

use std::ops::DerefMut;

use crate::env_info::{analytics_folder, is_analytics_disabled_by_env};
use crate::ga4_event::GA4Value;
use crate::ga4_metrics_service::*;
use crate::metrics_state::{MetricsState, UNKNOWN_VERSION};

const INIT_ERROR: &str = "Please call analytics::init prior to any other analytics api calls.";

pub static GA4_METRICS_INSTANCE: OnceLock<Arc<Mutex<GA4MetricsService>>> = OnceLock::new();

/// Initializes and return the G4 Metrics Service.
/// Only call this once, but, call it before calling
/// ga4_metrics().
pub async fn init_ga4_metrics_service(
    app_name: String,
    build_version: Option<String>,
    sdk_version: String,
    ga4_product_code: String,
    ga4_key: String,
    invoker: Option<String>,
) -> Result<Arc<Mutex<GA4MetricsService>>> {
    let metrics_state = MetricsState::from_config(
        &PathBuf::from(&analytics_folder()),
        app_name,
        build_version.unwrap_or(UNKNOWN_VERSION.into()),
        sdk_version,
        "deprecated".to_string(),
        ga4_product_code,
        ga4_key,
        is_analytics_disabled_by_env(),
        invoker,
    );
    let data = Mutex::new(GA4MetricsService::new(metrics_state));
    let svc = Arc::new(data);
    if let Err(_) = GA4_METRICS_INSTANCE.set(svc.clone()) {
        bail!(INIT_ERROR)
    }
    Ok(svc)
}

/// After calling init above once in your app,
/// use this to get an instance of the GA4MetricsService
/// whenever necessary.
pub async fn ga4_metrics() -> Result<impl DerefMut<Target = GA4MetricsService>> {
    if let Some(svc) = GA4_METRICS_INSTANCE.get() {
        Ok(svc.lock().await)
    } else {
        bail!(INIT_ERROR)
    }
}

/// Returns a legal notice of metrics data collection if user
/// is new to all tools (full notice) or new to this tool (brief notice).
/// Returns an error if init has not been called.
pub async fn get_notice() -> Option<String> {
    GA4_METRICS_INSTANCE.get()?.lock().await.get_notice()
}

/// Records intended opt in status.
/// Returns an error if init has not been called
pub async fn set_opt_in_status(enabled: bool) -> Result<()> {
    ga4_metrics().await?.set_opt_in_status(enabled)
}

/// Returns current opt in status.
/// Returns an error if init has not been called.
pub async fn is_opted_in() -> bool {
    ga4_metrics().await.is_ok_and(|s| s.is_opted_in())
}

/// Disable analytics for this invocation only.
/// This does not affect the global analytics state.
pub async fn opt_out_for_this_invocation() -> Result<()> {
    ga4_metrics().await?.opt_out_for_this_invocation()
}

/// Records a launch event with the command line args used to launch app.
/// Returns an error if init has not been called.
/// TODO(https://fxbug.dev/42077438) remove this once we remove UA and update foxtrot
pub async fn add_launch_event(args: Option<&str>) -> Result<()> {
    let mut ga4_svc = ga4_metrics().await?;
    ga4_svc.add_launch_event(args).await?;
    ga4_svc.send_events().await
}

/// Records an error event in the app.
/// Returns an error if init has not been called.
/// TODO(https://fxbug.dev/42077438) remove this once we remove UA
pub async fn add_crash_event(description: &str, fatal: Option<&bool>) -> Result<()> {
    let mut ga4_svc = ga4_metrics().await?;
    ga4_svc.add_crash_event(description, fatal).await?;
    ga4_svc.send_events().await
}

/// Records an event with an option to specify every parameter.
/// Returns an error if init has not been called.
/// TODO(https://fxbug.dev/42077438) remove this when UA is removed.
pub async fn add_custom_event(
    category: Option<&str>,
    action: Option<&str>,
    label: Option<&str>,
    custom_dimensions: BTreeMap<&str, GA4Value>,
) -> Result<()> {
    let mut ga4_svc = ga4_metrics().await?;
    ga4_svc.add_custom_event(category, action, label, custom_dimensions, category).await?;
    ga4_svc.send_events().await
}
