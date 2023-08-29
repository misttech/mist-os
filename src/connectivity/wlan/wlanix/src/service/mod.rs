// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        build_nl80211_ack, build_nl80211_done, build_nl80211_message,
        nl80211::{Nl80211Attr, Nl80211Cmd},
        nl80211_message_resp, WifiState,
    },
    anyhow::{Context, Error},
    async_trait::async_trait,
    fidl_fuchsia_wlan_wlanix as fidl_wlanix,
    parking_lot::Mutex,
    std::sync::Arc,
};

pub mod sme;

#[async_trait]
/// This trait represents all wlanix behavior that may be served by a different
/// underlying mechanism, e.g. SME vs wlancfg. Sensible defaults are provided
/// so that functionality can be added gradually to implementations.
/// The scope of this trait will grow considerably as new functionality is added.
pub(crate) trait WlanixService: Send + Sync {
    async fn trigger_nl80211_scan(
        &self,
        responder: fidl_wlanix::Nl80211MessageResponder,
        state: Arc<Mutex<WifiState>>,
    ) -> Result<(), Error> {
        responder
            .send(Ok(nl80211_message_resp(vec![build_nl80211_ack()])))
            .context("Failed to ack TriggerScan")?;

        if let Some(proxy) = state.lock().scan_multicast_proxy.as_ref() {
            proxy
                .message(fidl_wlanix::Nl80211MulticastMessageRequest {
                    message: Some(build_nl80211_message(
                        Nl80211Cmd::NewScanResults,
                        vec![Nl80211Attr::IfaceIndex(0)],
                    )),
                    ..Default::default()
                })
                .context("Failed to send NewScanResults")?;
        }
        Ok(())
    }

    fn get_nl80211_scan(
        &self,
        responder: fidl_wlanix::Nl80211MessageResponder,
    ) -> Result<(), Error> {
        // Send empty results.
        responder
            .send(Ok(nl80211_message_resp(vec![build_nl80211_done()])))
            .context("Failed to send scan results")
    }
}
