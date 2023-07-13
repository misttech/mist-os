// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxb/126764) Remove this file when UA is turned down (July 1, 2023)

use {
    anyhow::Result,
    fuchsia_hyper::{new_https_client, HttpsClient},
    futures::lock::Mutex,
    hyper::body::HttpBody,
    hyper::{Body, Method, Request},
    lazy_static::lazy_static,
    std::collections::BTreeMap,
    std::sync::Arc,
};

use crate::ga_event::*;
use crate::metrics_state::*;
use crate::MetricsEventBatch;

#[cfg(test)]
const GA_URL: &str = "https://www.google-analytics.com/debug/collect";
#[cfg(not(test))]
const GA_URL: &str = "https://www.google-analytics.com/collect";
const GA_BATCH_URL: &str = "https://www.google-analytics.com/batch";

lazy_static! {
    pub(crate) static ref METRICS_SERVICE: Arc<Mutex<MetricsService>> = Arc::new(Mutex::new(MetricsService::default()));
    // Note: lazy_static does not run any deconstructors.
}

/// The implementation of the metrics public api.
/// By default a new instance is uninitialized until
/// the state has been added.
pub(crate) struct MetricsService {
    pub(crate) init_state: MetricsServiceInitStatus,
    state: MetricsState,
    client: HttpsClient,
}

#[derive(Debug, PartialEq)]
pub(crate) enum MetricsServiceInitStatus {
    UNINITIALIZED,
    INITIALIZED,
}

impl MetricsService {
    pub(crate) fn inner_init(&mut self, state: MetricsState) {
        self.init_state = MetricsServiceInitStatus::INITIALIZED;
        self.state = state;
    }

    fn inner_is_opted_in(&self) -> bool {
        self.state.is_opted_in()
    }
    pub fn uuid_as_str(&self) -> String {
        self.state.uuid.map_or("No uuid".to_string(), |u| u.to_string())
    }

    pub(crate) fn make_batch(&self) -> MetricsEventBatch {
        MetricsEventBatch::new()
    }

    pub(crate) async fn inner_add_launch_event(
        &self,
        args: Option<&str>,
        batch_collector: Option<&mut MetricsEventBatch>,
    ) -> Result<()> {
        self.inner_add_custom_event(None, args, args, BTreeMap::new(), batch_collector).await
    }

    pub(crate) async fn inner_add_custom_event(
        &self,
        category: Option<&str>,
        action: Option<&str>,
        label: Option<&str>,
        custom_dimensions: BTreeMap<&str, String>,
        batch_collector: Option<&mut MetricsEventBatch>,
    ) -> Result<()> {
        if !self.inner_is_opted_in() {
            return Ok(());
        }
        let ua_event_body = self.make_ua_post_body(category, action, label, &custom_dimensions);
        self.handle_event(batch_collector, ua_event_body).await
    }

    // TODO should we add the extra events, subcommand, etc now that GA4 is more flexible?
    // UA format
    // fx exception in subcommand
    // "t=event" \
    // "ec=fx_exception" \
    // "ea=${subcommand}" \
    // "el=${args}" \
    // "cd1=${exit_status}" \
    // )
    pub(crate) async fn inner_add_crash_event(
        &self,
        description: &str,
        fatal: Option<&bool>,
        batch_collector: Option<&mut MetricsEventBatch>,
    ) -> Result<()> {
        if !self.inner_is_opted_in() {
            return Ok(());
        }
        let ua_event_body = self.make_ua_crash_body(&description, fatal);
        self.handle_event(batch_collector, ua_event_body).await
    }

    async fn handle_event(
        &self,
        batch_collector: Option<&mut MetricsEventBatch>,
        ua_event_body: String,
    ) -> std::result::Result<(), anyhow::Error> {
        match batch_collector {
            None => self.http_post_to_ua(ua_event_body, GA_URL).await,
            Some(bc) => {
                bc.add_event_string(ua_event_body);
                Ok(())
            }
        }
    }

    /// Records a timing event from the app.
    /// Returns an error if init has not been called.
    pub(crate) async fn inner_add_timing_event(
        &self,
        category: Option<&str>,
        time: String,
        variable: Option<&str>,
        label: Option<&str>,
        custom_dimensions: BTreeMap<&str, String>,
        batch_collector: Option<&mut MetricsEventBatch>,
    ) -> Result<()> {
        if !self.inner_is_opted_in() {
            return Ok(());
        }
        let ua_event_body = self.make_ua_timing_body(
            category,
            time.clone(),
            variable,
            label,
            custom_dimensions.clone(),
        );

        self.handle_event(batch_collector, ua_event_body).await
    }

    async fn http_post_to_ua(&self, post_body: String, url: &str) -> Result<(), anyhow::Error> {
        let req = Request::builder().method(Method::POST).uri(url).body(Body::from(post_body))?;
        let res = self.client.request(req).await;
        match res {
            Ok(mut res) => {
                tracing::debug!("UA Analytics response: {}", res.status());
                while let Some(chunk) = res.body_mut().data().await {
                    tracing::trace!(?chunk);
                }
            }
            Err(e) => tracing::warn!("Error posting UA analytics: {}", e),
        }
        Ok(())
    }

    fn make_ua_post_body(
        &self,
        category: Option<&str>,
        action: Option<&str>,
        label: Option<&str>,
        custom_dimensions: &BTreeMap<&str, String>,
    ) -> String {
        make_body_with_hash(
            &self.state.app_name,
            Some(&self.state.build_version),
            &self.state.ga_product_code,
            category,
            action,
            label,
            custom_dimensions.clone(),
            self.uuid_as_str(),
            self.state.invoker.as_ref(),
        )
    }

    fn make_ua_crash_body(&self, description: &str, fatal: Option<&bool>) -> String {
        make_crash_body_with_hash(
            &self.state.app_name,
            Some(&self.state.build_version),
            &self.state.ga_product_code,
            description,
            fatal,
            BTreeMap::new(),
            self.uuid_as_str(),
            self.state.invoker.as_ref(),
        )
    }

    /// Uses event parameters and property state to create post body for
    /// Google Analytics (Universal)
    fn make_ua_timing_body(
        &self,
        category: Option<&str>,
        time: String,
        variable: Option<&str>,
        label: Option<&str>,
        custom_dimensions: BTreeMap<&str, String>,
    ) -> String {
        make_timing_body_with_hash(
            &self.state.app_name,
            Some(&self.state.build_version),
            &self.state.ga_product_code,
            category,
            time,
            variable,
            label,
            custom_dimensions,
            self.uuid_as_str(),
            self.state.invoker.as_ref(),
        )
    }

    pub(crate) async fn send_ua_events(&self, body: String, batch: bool) -> Result<()> {
        if !self.inner_is_opted_in() {
            return Ok(());
        }
        let url = match batch {
            true => GA_BATCH_URL,
            false => GA_URL,
        };

        tracing::debug!(%url, %body, "POSTING UA ANALYTICS");
        self.http_post_to_ua(body, url).await
    }
}

impl Default for MetricsService {
    fn default() -> Self {
        Self {
            init_state: MetricsServiceInitStatus::UNINITIALIZED,
            state: MetricsState::default(),
            client: new_https_client(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics_state::write_opt_in_status;
    use crate::metrics_state::UNKNOWN_PROPERTY_ID;
    use std::path::PathBuf;
    use tempfile::tempdir;

    const APP_NAME: &str = "my cool app";
    const BUILD_VERSION: &str = "12/09/20 00:00:00";
    // const LAUNCH_ARGS: &str = "config analytics enable";

    fn test_metrics_svc(
        app_support_dir_path: &PathBuf,
        app_name: String,
        build_version: String,
        ga_product_code: String,
        ga4_product_code: String,
        ga4_key: String,
        disabled: bool,
    ) -> MetricsService {
        MetricsService {
            init_state: MetricsServiceInitStatus::INITIALIZED,
            state: MetricsState::from_config(
                app_support_dir_path,
                app_name,
                build_version,
                ga_product_code,
                ga4_product_code,
                ga4_key,
                disabled,
                None,
            ),
            client: new_https_client(),
        }
    }

    #[test]
    fn existing_user_first_use_of_this_tool() -> Result<()> {
        let dir = create_tmp_metrics_dir()?;
        write_opt_in_status(&dir, true)?;

        let ms = test_metrics_svc(
            &dir,
            String::from(APP_NAME),
            String::from(BUILD_VERSION),
            UNKNOWN_PROPERTY_ID.to_string(),
            UNKNOWN_GA4_PRODUCT_CODE.to_string(),
            UNKNOWN_GA4_KEY.to_string(),
            false,
        );

        assert_eq!(ms.state.status, MetricsStatus::NewToTool);
        drop(dir);
        Ok(())
    }

    pub fn create_tmp_metrics_dir() -> Result<PathBuf> {
        let tmp_dir = tempdir()?;
        let dir_obj = tmp_dir.path().join("fuchsia_metrics");
        let dir = dir_obj.as_path();
        Ok(dir.to_owned())
    }
}
