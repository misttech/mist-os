// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::GenericNetlinkProtocolServer;
use crate::logging::log_info;

use netlink::NETLINK_LOG_TAG;
use netlink_packet_core::NetlinkHeader;

pub struct Nl80211ProtocolServer {}

impl<S> GenericNetlinkProtocolServer<S> for Nl80211ProtocolServer {
    fn name(&self) -> String {
        "nl80211".into()
    }

    fn multicast_groups(&self) -> Vec<String> {
        vec![
            "scan".to_string(),
            "regulatory".to_string(),
            "mlme".to_string(),
            "vendor".to_string(),
            "config".to_string(),
        ]
    }

    fn handle_message(&self, netlink_header: NetlinkHeader, payload: Vec<u8>, _sender: &mut S) {
        log_info!(
            tag = NETLINK_LOG_TAG,
            "Received nl80211 netlink protocol message: {:?}  -- {:?}",
            netlink_header,
            payload
        );
    }
}
