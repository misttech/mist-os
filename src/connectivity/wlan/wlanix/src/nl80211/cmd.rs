// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use netlink_packet_utils::DecodeError;
use std::convert::TryFrom;

use crate::nl80211::constants::*;

#[derive(Clone, Copy, Eq, PartialEq, Debug)]
pub enum Nl80211Cmd {
    Unspecified,
    GetWiphy,
    SetWiphy,
    NewWiphy,
    DelWiphy,
    GetInterface,
    SetInterface,
    NewInterface,
    DelInterface,
    GetScan,
    TriggerScan,
    NewScanResults,
    ScanAborted,
    GetProtocolFeatures,
}

impl From<Nl80211Cmd> for u8 {
    fn from(cmd: Nl80211Cmd) -> u8 {
        use Nl80211Cmd::*;
        match cmd {
            Unspecified => NL80211_CMD_UNSPEC,
            GetWiphy => NL80211_CMD_GET_WIPHY,
            SetWiphy => NL80211_CMD_SET_WIPHY,
            NewWiphy => NL80211_CMD_NEW_WIPHY,
            DelWiphy => NL80211_CMD_DEL_WIPHY,
            GetInterface => NL80211_CMD_GET_INTERFACE,
            SetInterface => NL80211_CMD_SET_INTERFACE,
            NewInterface => NL80211_CMD_NEW_INTERFACE,
            DelInterface => NL80211_CMD_DEL_INTERFACE,
            GetScan => NL80211_CMD_GET_SCAN,
            TriggerScan => NL80211_CMD_TRIGGER_SCAN,
            NewScanResults => NL80211_CMD_NEW_SCAN_RESULTS,
            ScanAborted => NL80211_CMD_SCAN_ABORTED,
            GetProtocolFeatures => NL80211_CMD_GET_PROTOCOL_FEATURES,
        }
    }
}

impl TryFrom<u8> for Nl80211Cmd {
    type Error = DecodeError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        use Nl80211Cmd::*;
        Ok(match value {
            NL80211_CMD_UNSPEC => Unspecified,
            NL80211_CMD_GET_WIPHY => GetWiphy,
            NL80211_CMD_SET_WIPHY => SetWiphy,
            NL80211_CMD_NEW_WIPHY => NewWiphy,
            NL80211_CMD_DEL_WIPHY => DelWiphy,
            NL80211_CMD_GET_INTERFACE => GetInterface,
            NL80211_CMD_SET_INTERFACE => SetInterface,
            NL80211_CMD_NEW_INTERFACE => NewInterface,
            NL80211_CMD_DEL_INTERFACE => DelInterface,
            NL80211_CMD_GET_SCAN => GetScan,
            NL80211_CMD_TRIGGER_SCAN => TriggerScan,
            NL80211_CMD_NEW_SCAN_RESULTS => NewScanResults,
            NL80211_CMD_SCAN_ABORTED => ScanAborted,
            NL80211_CMD_GET_PROTOCOL_FEATURES => GetProtocolFeatures,
            other => {
                return Err(DecodeError::from(format!("Unhandled NL80211 command: {}", other)));
            }
        })
    }
}
