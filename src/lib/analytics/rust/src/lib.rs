// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod env_info;
mod ga4_event;
mod ga4_metrics_service;
mod ga_event;
pub mod metrics_event_batch;
mod metrics_service;

mod metrics_state;
mod notice;

use {
    anyhow::{bail, Result},
    futures::lock::Mutex,
    std::collections::BTreeMap,
    std::path::PathBuf,
    std::sync::Arc,
    std::sync::OnceLock,
    tracing,
};

use std::ops::DerefMut;

use crate::env_info::{analytics_folder, is_analytics_disabled_by_env};
use crate::ga4_event::convert_to_ga4values;
use crate::ga4_metrics_service::*;
use crate::metrics_event_batch::MetricsEventBatch;
use crate::metrics_service::*;
use crate::metrics_state::{MetricsState, UNKNOWN_VERSION};

const INIT_ERROR: &str = "Please call analytics::init prior to any other analytics api calls.";

pub static GA4_METRICS_INSTANCE: OnceLock<Arc<Mutex<GA4MetricsService>>> = OnceLock::new();

/// This function initializes the metrics service so that an app
/// can make posts to the analytics service and read the current opt in status of the user
/// TODO(fxb/126764) remove this function once we remove UA 4 and refactor all clients.
pub async fn init_with_invoker(
    app_name: String,
    build_version: Option<String>,
    ga_product_code: String,
    ga4_product_code: String,
    ga4_key: String,
    invoker: Option<String>,
) {
    let metrics_state = MetricsState::from_config(
        &PathBuf::from(&analytics_folder()),
        app_name,
        build_version.unwrap_or(UNKNOWN_VERSION.into()),
        ga_product_code,
        ga4_product_code,
        ga4_key,
        is_analytics_disabled_by_env(),
        invoker,
    );

    // TODO(fxb/126764) Remove this line when UA is turned down (July 1, 2023)
    // We are running both versions of the service until July 1 since the
    // protocols are organized differently enough to make it worth creating two
    // different services.
    METRICS_SERVICE.lock().await.inner_init(metrics_state.clone());

    let _ = GA4_METRICS_INSTANCE
        .set(Arc::new(Mutex::new(GA4MetricsService::new(metrics_state.clone()))));
}

/// This function initializes the metrics service with a default of NONE for the invoker
pub async fn init_ga4(
    app_name: String,
    build_version: Option<String>,
    ga_product_code: String,
    ga4_product_code: String,
    ga4_key: String,
) {
    init_with_invoker(app_name, build_version, ga_product_code, ga4_product_code, ga4_key, None)
        .await;
}

/// This function initializes the metrics service with a default of NONE for the invoker
/// TODO remove this function after change to dependent petal updates to init_ga4 above.
pub async fn init(app_name: String, build_version: Option<String>, ga_product_code: String) {
    init_with_invoker(
        app_name,
        build_version,
        ga_product_code,
        "UNKNOWN_GA4_PROPERTY".to_string(),
        "UNKNOWN_GA4_KEY".to_string(),
        None,
    )
    .await;
}

/// Initializes and return the G4 Metrics Service.
/// TODO(fxb/126764) refactor all clients to use this once we remove UA analytics
pub async fn init_ga4_metrics_service(
    app_name: String,
    build_version: Option<String>,
    ga4_product_code: String,
    ga4_key: String,
    invoker: Option<String>,
) -> Result<Arc<Mutex<GA4MetricsService>>> {
    let metrics_state = MetricsState::from_config(
        &PathBuf::from(&analytics_folder()),
        app_name,
        build_version.unwrap_or(UNKNOWN_VERSION.into()),
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

/// Returns a legal notice of metrics data collection if user
/// is new to all tools (full notice) or new to this tool (brief notice).
/// Returns an error if init has not been called.
/// TODO(fxb/126764) remove this once we remove UA
pub async fn get_notice() -> Option<String> {
    GA4_METRICS_INSTANCE.get()?.lock().await.get_notice()
}

async fn ga4_metrics() -> Result<impl DerefMut<Target = GA4MetricsService>> {
    if let Some(svc) = GA4_METRICS_INSTANCE.get() {
        Ok(svc.lock().await)
    } else {
        bail!(INIT_ERROR)
    }
}
/// Records intended opt in status.
/// Returns an error if init has not been called
/// TODO(fxb/126764) remove this once we remove UA
pub async fn set_opt_in_status(enabled: bool) -> Result<()> {
    ga4_metrics().await?.set_opt_in_status(enabled)
}

/// Returns current opt in status.
/// Returns an error if init has not been called.
/// TODO(fxb/126764) remove this once we remove UA
pub async fn is_opted_in() -> bool {
    ga4_metrics().await.is_ok_and(|s| s.is_opted_in())
}

/// Disable analytics for this invocation only.
/// This does not affect the global analytics state.
/// TODO(fxb/126764) remove this once we remove UA
pub async fn opt_out_for_this_invocation() -> Result<()> {
    ga4_metrics().await?.opt_out_for_this_invocation()
}

/// Records a launch event with the command line args used to launch app.
/// Returns an error if init has not been called.
/// TODO(fxb/126764) remove this once we remove UA
pub async fn add_launch_event(args: Option<&str>) -> Result<()> {
    let svc = METRICS_SERVICE.lock().await;
    match &svc.init_state {
        MetricsServiceInitStatus::INITIALIZED => {
            svc.inner_add_launch_event(args, None).await?;
        }
        MetricsServiceInitStatus::UNINITIALIZED => {
            tracing::error!("add_launch_event called on uninitialized METRICS_SERVICE");
            bail!(INIT_ERROR)
        }
    }

    let mut ga4_svc = ga4_metrics().await?;
    ga4_svc.add_launch_event(args).await?;
    ga4_svc.send_events().await
}

/// Records an error event in the app.
/// Returns an error if init has not been called.
/// TODO(fxb/126764) remove this once we remove UA
pub async fn add_crash_event(description: &str, fatal: Option<&bool>) -> Result<()> {
    let svc = METRICS_SERVICE.lock().await;
    match &svc.init_state {
        MetricsServiceInitStatus::INITIALIZED => {
            svc.inner_add_crash_event(description, fatal, None).await?;
        }
        MetricsServiceInitStatus::UNINITIALIZED => {
            tracing::error!("add_crash_event called on uninitialized METRICS_SERVICE");
            bail!(INIT_ERROR)
        }
    }
    let mut ga4_svc = ga4_metrics().await?;
    ga4_svc.add_crash_event(description, fatal).await?;
    ga4_svc.send_events().await
}

/// Records a timing event from the app.
/// Returns an error if init has not been called.
/// TODO(fxb/126764) remove this once we remove UA
pub async fn add_timing_event(
    category: Option<&str>,
    duration_str: String,
    variable: Option<&str>,
    label: Option<&str>,
    custom_dimensions: BTreeMap<&str, String>,
) -> Result<()> {
    let svc = METRICS_SERVICE.lock().await;
    match &svc.init_state {
        MetricsServiceInitStatus::INITIALIZED => {
            svc.inner_add_timing_event(
                category,
                duration_str.clone(),
                variable,
                label,
                custom_dimensions.clone(),
                None,
            )
            .await?;
        }
        MetricsServiceInitStatus::UNINITIALIZED => {
            tracing::error!("add_timing_event called on uninitialized METRICS_SERVICE");
            bail!(INIT_ERROR)
        }
    }

    let duration = match duration_str.parse() {
        Ok(t) => t,
        Err(e) => bail!("Unable to process time {}", e),
    };
    let custom_dimensions_ga4 = convert_to_ga4values(custom_dimensions);
    let mut ga4_svc = ga4_metrics().await?;
    ga4_svc.add_timing_event(category, duration, variable, label, custom_dimensions_ga4).await?;
    ga4_svc.send_events().await
}

/// Records an event with an option to specify every parameter.
/// Returns an error if init has not been called.
/// TODO(fxb/126764) remove this when UA is removed.
pub async fn add_custom_event(
    category: Option<&str>,
    action: Option<&str>,
    label: Option<&str>,
    custom_dimensions: BTreeMap<&str, String>,
) -> Result<()> {
    let svc = METRICS_SERVICE.lock().await;
    match &svc.init_state {
        MetricsServiceInitStatus::INITIALIZED => {
            svc.inner_add_custom_event(category, action, label, custom_dimensions.clone(), None)
                .await?;
        }
        MetricsServiceInitStatus::UNINITIALIZED => {
            tracing::error!("add_custom_event called on uninitialized METRICS_SERVICE");
            bail!(INIT_ERROR)
        }
    }

    let custom_dimensions_ga4 = convert_to_ga4values(custom_dimensions);
    let mut ga4_svc = ga4_metrics().await?;
    ga4_svc.add_custom_event(category, action, label, custom_dimensions_ga4, category).await?;
    ga4_svc.send_events().await
}

/// TODO(fxb/126764) drop when we remove UA
pub async fn make_batch() -> Result<MetricsEventBatch> {
    let svc = METRICS_SERVICE.lock().await;
    match &svc.init_state {
        MetricsServiceInitStatus::INITIALIZED => Ok(svc.make_batch()),
        MetricsServiceInitStatus::UNINITIALIZED => {
            tracing::error!("make_batch called on uninitialized METRICS_SERVICE");
            bail!(INIT_ERROR)
        }
    }
}

/// TODO(fxb/126764) use this fn once we delete UA analytics and remove MetricsEventBatch.
/// Until then, MetricsEventBatch will call GA4METRICS_SERVICE.send_events.
pub async fn send_events() -> Result<()> {
    // TODO(fxb/126764) after dropping UA, change this to Post::add_event followed by Post::send_events separately
    ga4_metrics().await?.send_events().await
}

/// This is exposed for clients who want to use the uuid as a custom dimension
/// to do more accurate user counts in DataStudio analyses.
/// TODO(fxb/126764) Remove this when we remove UA analytics since it will no longer be necessary.
pub async fn uuid_as_str() -> Result<String> {
    let svc = METRICS_SERVICE.lock().await;
    match &svc.init_state {
        MetricsServiceInitStatus::INITIALIZED => Ok(svc.uuid_as_str()),
        MetricsServiceInitStatus::UNINITIALIZED => {
            tracing::error!("uuid_as_str called on uninitialized METRICS_SERVICE");
            bail!(INIT_ERROR)
        }
    }
}
