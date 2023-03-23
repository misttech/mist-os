// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::common_utils::common::macros::{fx_err_and_bail, with_line};
use crate::media_session::types::PlayerStatusWrapper;
use anyhow::Error;
use fidl_fuchsia_media_sessions2::{ActiveSessionMarker, ActiveSessionProxy};
use fuchsia_component::client::connect_to_protocol;

#[derive(Debug)]
pub struct MediaSessionFacade {
    active_session_proxy: Option<ActiveSessionProxy>,
}

impl MediaSessionFacade {
    pub fn new() -> Self {
        Self { active_session_proxy: None }
    }

    /// Returns the active session proxy.
    fn get_active_session_proxy(&self) -> Result<ActiveSessionProxy, Error> {
        if let Some(proxy) = &self.active_session_proxy {
            Ok(proxy.clone())
        } else {
            match connect_to_protocol::<ActiveSessionMarker>() {
                Ok(proxy) => Ok(proxy),
                Err(e) => fx_err_and_bail!(
                    &with_line!("MediaSessionFacade::get_active_session_proxy"),
                    format_err!("Failed to create proxy: {:?}", e)
                ),
            }
        }
    }

    /// Returns the active media session's player status.
    /// If there's no active session, it will return a None type value.
    pub async fn watch_active_session_status(&self) -> Result<Option<PlayerStatusWrapper>, Error> {
        let active_session_proxy = &self
            .get_active_session_proxy()
            .map_err(|e| format_err!("Failed to get active session proxy: {}", e))?;
        let session = active_session_proxy
            .watch_active_session()
            .await
            .map_err(|e| format_err!("Failed to watch active session: {}", e))?;
        if let Some(session_control_marker) = session {
            let info_delta = session_control_marker
                .into_proxy()
                .map_err(|e| format_err!("Failed to get session control proxy: {}", e))?
                .watch_status()
                .await
                .map_err(|e| format_err!("Failed to watch session status: {}", e))?;
            Ok(Some(info_delta.player_status.into()))
        } else {
            Ok(None)
        }
    }
}
