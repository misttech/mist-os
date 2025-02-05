// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_bluetooth::types::PeerId;
use regex::Regex;
use std::fmt;
use std::str::FromStr;

/// A Bluetooth MAC address: 6 bytes written in hexadecimal and separated by colons.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct BdAddr(pub String);

impl FromStr for BdAddr {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let address_pattern = Regex::new(r"^([0-9A-Fa-f]{2}[:-]){5}([0-9A-Fa-f]{2})$")
            .expect("Could not compile MAC address regex pattern.");
        if address_pattern.is_match(s) {
            Ok(Self(s.to_string()))
        } else {
            Err("Not a valid MAC address".to_string())
        }
    }
}

impl fmt::Display for BdAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerIdOrAddr {
    PeerId(PeerId),
    BdAddr(BdAddr),
}

impl FromStr for PeerIdOrAddr {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Ok(addr) = BdAddr::from_str(s) {
            return Ok(PeerIdOrAddr::BdAddr(addr));
        }
        if let Ok(id) = PeerId::from_str(s) {
            return Ok(PeerIdOrAddr::PeerId(id));
        }
        Err("Not a valid Peer ID or MAC address".to_string())
    }
}
impl fmt::Display for PeerIdOrAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PeerIdOrAddr::PeerId(id) => write!(f, "{}", id),
            PeerIdOrAddr::BdAddr(addr) => write!(f, "{}", addr),
        }
    }
}
