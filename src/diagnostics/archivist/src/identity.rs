// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_message::MonikerWithUrl;
use flyweights::FlyStr;
use moniker::ExtendedMoniker;
use std::string::ToString;

#[derive(Debug, Eq, Hash, PartialEq)]
pub struct ComponentIdentity {
    /// Moniker of the component that this artifacts container is representing.
    pub moniker: ExtendedMoniker,

    /// The url with which the associated component was launched.
    pub url: FlyStr,
}

impl ComponentIdentity {
    pub fn new(moniker: ExtendedMoniker, url: impl Into<FlyStr>) -> Self {
        ComponentIdentity { moniker, url: url.into() }
    }

    /// Returns generic metadata, suitable for providing a uniform ID to unattributed data.
    pub fn unknown() -> Self {
        Self::new(
            ExtendedMoniker::parse_str("/UNKNOWN").expect("Unknown is valid"),
            "fuchsia-pkg://UNKNOWN",
        )
    }
}

#[cfg(test)]
impl From<Vec<&str>> for ComponentIdentity {
    fn from(moniker_segments: Vec<&str>) -> Self {
        let moniker = moniker::Moniker::try_from(moniker_segments).unwrap();
        Self { moniker: ExtendedMoniker::from(moniker), url: "".into() }
    }
}

impl From<ComponentIdentity> for MonikerWithUrl {
    fn from(identity: ComponentIdentity) -> Self {
        Self { moniker: identity.to_string(), url: identity.url.into() }
    }
}

impl From<&ComponentIdentity> for MonikerWithUrl {
    fn from(identity: &ComponentIdentity) -> Self {
        Self { moniker: identity.to_string(), url: identity.url.to_string() }
    }
}

impl std::fmt::Display for ComponentIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.moniker.fmt(f)
    }
}
