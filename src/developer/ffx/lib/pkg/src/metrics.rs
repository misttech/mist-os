// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use analytics::add_custom_event;
use fuchsia_async::TimeoutExt as _;
use fuchsia_repo::repository::RepositorySpec;
use std::collections::BTreeMap;
use std::time::Duration;

const CATEGORY: &str = "ffx_daemon_repo";

async fn add_event(action: &'static str, label: Option<String>) {
    let analytics_task = fuchsia_async::Task::local(async move {
        match add_custom_event(Some(CATEGORY), Some(&action), label.as_deref(), BTreeMap::new())
            .await
        {
            Ok(_) => {}
            Err(err) => {
                tracing::error!("metrics submission failed: {}", err);
            }
        }
    });

    analytics_task
        .on_timeout(Duration::from_secs(2), || {
            tracing::error!("metrics submisson timed out");
        })
        .await;
}

// TODO(fxbug/127781) Change to pub(crate) once repo library moves to this crate.
pub async fn server_mode_event(mode: &str) {
    add_event("server.mode", Some(mode.into())).await
}

// TODO(fxbug/127781) Change to pub(crate) once repo library moves to this crate.
pub async fn server_started_event() {
    add_event("server.state", Some("started".into())).await
}

// TODO(fxbug/127781) Change to pub(crate) once repo library moves to this crate.
pub async fn server_failed_to_start_event(msg: &str) {
    add_event("server.state", Some(msg.into())).await
}

// TODO(fxbug/127781) Change to pub(crate) once repo library moves to this crate.
pub async fn server_disabled_event() {
    add_event("server.state", Some("disabled".into())).await
}

// TODO(fxbug/127781) Change to pub(crate) once repo library moves to this crate.
pub async fn add_repository_event(repo_spec: &RepositorySpec) {
    let repo_type = match repo_spec {
        RepositorySpec::FileSystem { .. } => "filesystem",
        RepositorySpec::Pm { .. } => "pm",
        RepositorySpec::Http { .. } => "http",
        RepositorySpec::Gcs { .. } => "gcs",
    };

    add_event("protocol.add-repository", Some(repo_type.into())).await
}

// TODO(fxbug/127781) Change to pub(crate) once repo library moves to this crate.
pub async fn remove_repository_event() {
    add_event("protocol.remove-repository", None).await
}

// TODO(fxbug/127781) Change to pub(crate) once repo library moves to this crate.
pub async fn register_repository_event() {
    add_event("protocol.register-repository-to-target", None).await
}

// TODO(fxbug/127781) Change to pub(crate) once repo library moves to this crate.
pub async fn deregister_repository_event() {
    add_event("protocol.deregister-repository-from-target", None).await
}
