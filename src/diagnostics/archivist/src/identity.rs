// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::events::types::{ComponentIdentifier, Moniker, UniqueKey};
use diagnostics_message::MonikerWithUrl;
use flyweights::FlyStr;

#[derive(Debug, Eq, PartialEq)]
pub struct ComponentIdentity {
    /// Relative moniker of the component that this artifacts container
    /// is representing.
    pub relative_moniker: Moniker,

    /// Instance id (only set for v1 components)
    pub instance_id: Option<Box<str>>,

    /// The url with which the associated component was launched.
    pub url: FlyStr,
}

impl ComponentIdentity {
    pub fn from_identifier_and_url(
        identifier: ComponentIdentifier,
        url: impl Into<FlyStr>,
    ) -> Self {
        ComponentIdentity {
            relative_moniker: identifier.relative_moniker_for_selectors(),
            instance_id: match identifier {
                ComponentIdentifier::Legacy { instance_id, .. } => Some(instance_id),
                ComponentIdentifier::Moniker(..) => None,
            },
            url: url.into(),
        }
    }

    /// Returns generic metadata, suitable for providing a uniform ID to unattributed data.
    pub fn unknown() -> Self {
        Self::from_identifier_and_url(
            ComponentIdentifier::Legacy {
                instance_id: "0".into(),
                moniker: vec!["UNKNOWN"].into(),
            },
            "fuchsia-pkg://UNKNOWN",
        )
    }

    /// In V1, a component topology is able to produce two components with
    /// the same relative moniker. Because of this, we must, in some cases,
    /// differentiate these components using instance ids. The unique key
    /// is conceptually a relative moniker which preserves instance ids.
    pub fn unique_key(&self) -> UniqueKey {
        let mut key = self.relative_moniker.iter().cloned().collect::<Vec<_>>();
        if let Some(instance_id) = &self.instance_id {
            key.push(instance_id.into())
        }
        key.into()
    }
}

#[cfg(test)]
impl From<Vec<&str>> for ComponentIdentity {
    fn from(moniker_segments: Vec<&str>) -> Self {
        Self { relative_moniker: moniker_segments.into(), instance_id: None, url: "".into() }
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
        self.relative_moniker.fmt(f)
    }
}
