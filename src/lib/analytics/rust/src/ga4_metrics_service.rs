// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Result,
    fuchsia_hyper::{new_https_client, HttpsClient},
    hyper::body::HttpBody,
    hyper::{Body, Method, Request},
    std::collections::{BTreeMap, HashMap},
};

use crate::{
    env_info::{get_arch, get_os},
    ga4_event::*,
    metrics_state::*,
    notice::{BRIEF_NOTICE, FULL_NOTICE},
};

const DOMAIN: &str = "www.google-analytics.com";
const ENDPOINT: &str = "/mp/collect";

/// The implementation of the GA4 Measurement Protocol metrics public api.
#[derive(Clone)]
pub struct GA4MetricsService {
    state: MetricsState,
    client: HttpsClient,
    post: Post,
}

impl GA4MetricsService {
    pub(crate) fn new(state: MetricsState) -> Self {
        let mut svc =
            GA4MetricsService { state: state, client: new_https_client(), post: Post::default() };
        svc.init_post();
        svc
    }

    /// Returns Analytics disclosure notice according to PDD rules.
    pub(crate) fn get_notice(&self) -> Option<String> {
        match self.state.status {
            MetricsStatus::NewUser => Some(FULL_NOTICE.to_string()),
            MetricsStatus::NewToTool => Some(BRIEF_NOTICE.to_string()),
            _ => None,
        }
    }

    /// Records Analytics participation status.
    pub(crate) fn set_opt_in_status(&mut self, enabled: bool) -> Result<()> {
        self.state.set_opt_in_status(enabled)
    }

    /// Returns Analytics participation status.
    pub(crate) fn is_opted_in(&self) -> bool {
        self.state.is_opted_in()
    }

    /// Disables analytics for this invocation only.
    /// This does not affect the global analytics state.
    pub fn opt_out_for_this_invocation(&mut self) -> Result<()> {
        self.state.opt_out_for_this_invocation()
    }

    /// Adds a launch event to the Post
    pub(crate) async fn add_launch_event(&mut self, args: Option<&str>) -> Result<()> {
        self.add_custom_event(None, args, args, BTreeMap::new(), Some("launch")).await
    }

    /// Adds an event to the post with open-ended parameters
    /// while still honoring the UA Event parameters already
    /// in use.
    pub(crate) async fn add_custom_event(
        &mut self,
        category: Option<&str>,
        action: Option<&str>,
        label: Option<&str>,
        custom_dimensions: BTreeMap<&str, GA4Value>,
        event_name: Option<&str>,
    ) -> Result<()> {
        if !self.is_opted_in() {
            return Ok(());
        }
        let ga4_event = make_ga4_event(
            category,
            action,
            label,
            custom_dimensions,
            self.state.invoker.as_deref(),
            event_name,
        );
        self.post.add_event(ga4_event);
        Ok(())
    }

    /// Adds a crash/exception event to the post
    /// conforming to the UA Event parameters already
    /// in use.
    // TODO With GA4's flexibility, rework exception reporting to be more informative
    pub(crate) async fn add_crash_event(
        &mut self,
        description: &str,
        fatal: Option<&bool>,
    ) -> Result<()> {
        if !self.is_opted_in() {
            return Ok(());
        }
        let ga4_event = make_ga4_crash_event(description, fatal, self.state.invoker.as_deref());
        self.post.add_event(ga4_event);
        Ok(())
    }

    /// Records a timing event from the app.
    pub(crate) async fn add_timing_event(
        &mut self,
        category: Option<&str>,
        time: u64,
        variable: Option<&str>,
        label: Option<&str>,
        custom_dimensions: BTreeMap<&str, GA4Value>,
    ) -> Result<()> {
        if !self.is_opted_in() {
            return Ok(());
        }
        let ga4_event = make_ga4_timing_event(
            category,
            time,
            variable,
            label,
            custom_dimensions,
            self.state.invoker.as_deref(),
        );
        self.post.add_event(ga4_event);
        Ok(())
    }

    /// Sends the Post, with all accumulated events
    /// to the Google Analytics service.
    pub(crate) async fn send_events(&mut self) -> Result<()> {
        if !self.is_opted_in() {
            return Ok(());
        }
        self.rewrite_ua_ffx_known_batch_to_ga4();
        let _ = self.post.validate()?;
        let post_body = self.post.to_json();
        let url = self.get_url();
        tracing::debug!(%url, %post_body, "POSTING GA4 ANALYTICS");

        let req = Request::builder()
            .method(Method::POST)
            .uri(url)
            .header("Content-Type", "application/json")
            .body(Body::from(post_body))?;
        let res = self.client.request(req).await;
        Ok(match res {
            Ok(mut res) => {
                tracing::debug!("GA 4 Analytics response: {}", res.status());
                while let Some(chunk) = res.body_mut().data().await {
                    tracing::trace!(?chunk);
                }
            }
            Err(e) => tracing::warn!("Error posting GA 4 analytics: {}", e),
        })
    }

    /// Rewrites the batch call from ffx invoke under UA analytics
    /// to a single event under GA4 Analytics.
    /// TODO Remove this once we remove UA analtyics and the ffx client has been updated
    /// to speak to the GA4 Metrics Service.
    fn rewrite_ua_ffx_known_batch_to_ga4(&mut self) {
        if self.post.events.len() == 2
            && self.post.events[0].name.eq_ignore_ascii_case("invoke")
            && self.post.events[1].name.eq_ignore_ascii_case("timing")
        {
            tracing::debug!("Rewriting ffx batch invoke to ga4 post invoke");
            let events = &mut self.post.events;
            let invoke_event = &mut events[0].clone();
            let timing_event = events.remove(1);
            if let Some(params) = timing_event.params {
                if params.params.contains_key("time") {
                    let time = params.params["time"].clone();
                    invoke_event.add_param("timing", time);
                }
                self.post.events = vec![invoke_event.to_owned()];
            }
        }
    }

    fn uuid_as_str(&self) -> String {
        self.state.uuid.map_or("No uuid".to_string(), |u| u.to_string())
    }

    /// Create the GA4 Post object that will be sent to Google Analytics.
    fn init_post(&mut self) {
        self.post = Post::new(self.uuid_as_str(), None, Some(self.make_user_properties()), vec![]);
    }

    /// Initialize the UserProperties to be sent to GA4 with events.
    fn make_user_properties(&self) -> HashMap<String, ValueObject> {
        HashMap::from([
            ("version".into(), ValueObject { value: self.state.build_version.clone().into() }),
            ("os".into(), ValueObject { value: get_os().into() }),
            ("arch".into(), ValueObject { value: get_arch().into() }),
        ])
    }

    fn get_url(&self) -> String {
        format!(
            "https://{}{}?api_secret={}&measurement_id={}",
            DOMAIN, ENDPOINT, self.state.ga4_key, self.state.ga4_product_code
        )
    }
}

impl Default for GA4MetricsService {
    fn default() -> Self {
        Self { state: MetricsState::default(), client: new_https_client(), post: Post::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics_state::write_app_status;
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
    ) -> GA4MetricsService {
        GA4MetricsService {
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
            post: Post::default(),
        }
    }

    #[test]
    fn new_user_of_any_tool() -> Result<()> {
        let dir = create_tmp_metrics_dir()?;
        let ms = test_metrics_svc(
            &dir,
            String::from(APP_NAME),
            String::from(BUILD_VERSION),
            UNKNOWN_PROPERTY_ID.to_string(),
            UNKNOWN_GA4_PRODUCT_CODE.to_string(),
            UNKNOWN_GA4_KEY.to_string(),
            false,
        );

        assert_eq!(ms.get_notice(), Some(FULL_NOTICE.replace("{app_name}", APP_NAME)));

        drop(dir);
        Ok(())
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
        assert_eq!(ms.get_notice(), Some(BRIEF_NOTICE.replace("{app_name}", APP_NAME)));
        drop(dir);
        Ok(())
    }

    #[test]
    fn existing_user_of_this_tool_opted_in() -> Result<()> {
        let dir = create_tmp_metrics_dir()?;
        write_opt_in_status(&dir, true)?;
        write_app_status(&dir, &APP_NAME, true)?;
        let ms = test_metrics_svc(
            &dir,
            String::from(APP_NAME),
            String::from(BUILD_VERSION),
            UNKNOWN_PROPERTY_ID.to_string(),
            UNKNOWN_GA4_PRODUCT_CODE.to_string(),
            UNKNOWN_GA4_KEY.to_string(),
            false,
        );

        assert_eq!(ms.get_notice(), None);
        drop(dir);
        Ok(())
    }

    #[test]
    fn existing_user_of_this_tool_opted_out() -> Result<()> {
        let dir = create_tmp_metrics_dir()?;
        write_opt_in_status(&dir, false)?;
        write_app_status(&dir, &APP_NAME, true)?;
        let ms = test_metrics_svc(
            &dir,
            String::from(APP_NAME),
            String::from(BUILD_VERSION),
            UNKNOWN_PROPERTY_ID.to_string(),
            UNKNOWN_GA4_PRODUCT_CODE.to_string(),
            UNKNOWN_GA4_KEY.to_string(),
            false,
        );

        assert_eq!(ms.get_notice(), None);

        drop(dir);
        Ok(())
    }

    #[test]
    fn with_disable_env_var_set() -> Result<()> {
        let dir = create_tmp_metrics_dir()?;
        write_opt_in_status(&dir, true)?;
        write_app_status(&dir, &APP_NAME, true)?;

        let ms = test_metrics_svc(
            &dir,
            String::from(APP_NAME),
            String::from(BUILD_VERSION),
            UNKNOWN_PROPERTY_ID.to_string(),
            UNKNOWN_GA4_PRODUCT_CODE.to_string(),
            UNKNOWN_GA4_KEY.to_string(),
            true,
        );

        assert_eq!(ms.get_notice(), None);

        drop(dir);
        Ok(())
    }

    #[test]
    fn opt_out_for_this_invocation() -> Result<()> {
        let dir = create_tmp_metrics_dir()?;
        let mut ms = test_metrics_svc(
            &dir,
            String::from(APP_NAME),
            String::from(BUILD_VERSION),
            UNKNOWN_PROPERTY_ID.to_string(),
            UNKNOWN_GA4_PRODUCT_CODE.to_string(),
            UNKNOWN_GA4_KEY.to_string(),
            false,
        );

        assert_eq!(ms.state.status, MetricsStatus::NewUser);
        let _res = ms.opt_out_for_this_invocation().unwrap();
        assert_eq!(ms.state.status, MetricsStatus::OptedOut);

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
