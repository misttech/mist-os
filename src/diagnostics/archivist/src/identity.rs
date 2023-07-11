// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_message::MonikerWithUrl;
use fidl_fuchsia_diagnostics::{ComponentSelector, Selector};
use flyweights::FlyStr;
use moniker::{AbsoluteMoniker, AbsoluteMonikerBase, ExtendedMoniker};
use std::{borrow::Borrow, string::ToString};

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
        let abs_moniker = AbsoluteMoniker::try_from(moniker_segments).unwrap();
        Self { moniker: ExtendedMoniker::from(abs_moniker), url: "".into() }
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

pub(crate) trait MonikerHelper {
    fn match_against_selectors<'a, S>(
        &self,
        selectors: &'a [S],
    ) -> Result<Vec<&'a Selector>, anyhow::Error>
    where
        S: Borrow<Selector>;

    fn match_against_component_selectors<'a, S>(
        &self,
        selectors: &'a [S],
    ) -> Result<Vec<&'a ComponentSelector>, anyhow::Error>
    where
        S: Borrow<ComponentSelector>;

    fn matches_selector(&self, selector: &Selector) -> Result<bool, anyhow::Error>;

    fn matches_component_selector(
        &self,
        selector: &ComponentSelector,
    ) -> Result<bool, anyhow::Error>;

    fn sanitized(&self) -> String;
}

const COMPONENT_MANAGER_MONIKER: &str = "<component_manager>";

impl MonikerHelper for ExtendedMoniker {
    fn match_against_selectors<'a, S>(
        &self,
        selectors: &'a [S],
    ) -> Result<Vec<&'a Selector>, anyhow::Error>
    where
        S: Borrow<Selector>,
    {
        match self {
            ExtendedMoniker::ComponentManager => {
                selectors::match_component_moniker_against_selectors(
                    &[COMPONENT_MANAGER_MONIKER],
                    selectors,
                )
            }
            ExtendedMoniker::ComponentInstance(moniker) => {
                let s = segments(moniker).collect::<Vec<_>>();
                selectors::match_component_moniker_against_selectors(&s, selectors)
            }
        }
    }

    fn match_against_component_selectors<'a, S>(
        &self,
        selectors: &'a [S],
    ) -> Result<Vec<&'a ComponentSelector>, anyhow::Error>
    where
        S: Borrow<ComponentSelector>,
    {
        match self {
            ExtendedMoniker::ComponentManager => {
                selectors::match_moniker_against_component_selectors(
                    &[COMPONENT_MANAGER_MONIKER],
                    selectors,
                )
            }
            ExtendedMoniker::ComponentInstance(moniker) => {
                let s = segments(moniker).collect::<Vec<_>>();
                selectors::match_moniker_against_component_selectors(&s, selectors)
            }
        }
    }

    fn matches_selector(&self, selector: &Selector) -> Result<bool, anyhow::Error> {
        match self {
            ExtendedMoniker::ComponentManager => {
                selectors::match_component_moniker_against_selector(
                    &[COMPONENT_MANAGER_MONIKER],
                    selector,
                )
            }
            ExtendedMoniker::ComponentInstance(moniker) => {
                let s = segments(moniker).collect::<Vec<_>>();
                selectors::match_component_moniker_against_selector(&s, selector)
            }
        }
    }

    fn matches_component_selector(
        &self,
        selector: &ComponentSelector,
    ) -> Result<bool, anyhow::Error> {
        match self {
            ExtendedMoniker::ComponentManager => {
                selectors::match_moniker_against_component_selector(
                    [COMPONENT_MANAGER_MONIKER].into_iter(),
                    selector,
                )
            }
            ExtendedMoniker::ComponentInstance(moniker) => {
                let s = segments(moniker).collect::<Vec<_>>();
                selectors::match_moniker_against_component_selector(s.into_iter(), selector)
            }
        }
    }

    fn sanitized(&self) -> String {
        match self {
            ExtendedMoniker::ComponentManager => COMPONENT_MANAGER_MONIKER.to_string(),
            ExtendedMoniker::ComponentInstance(moniker) => segments(moniker)
                .map(|s| selectors::sanitize_string_for_selectors(&s).into_owned())
                .collect::<Vec<String>>()
                .join("/"),
        }
    }
}

enum SegmentIterator<I> {
    Iter(I),
    Root(bool),
}

impl<I> Iterator for SegmentIterator<I>
where
    I: ExactSizeIterator<Item = String>,
{
    type Item = I::Item;
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Iter(iter) => iter.next(),
            Self::Root(true) => None,
            Self::Root(done) => {
                *done = true;
                Some("<root>".to_string())
            }
        }
    }
}

fn segments(moniker: &AbsoluteMoniker) -> impl Iterator<Item = String> + '_ {
    let iter = moniker.path().iter().map(|s| s.to_string());
    if iter.len() == 0 {
        return SegmentIterator::Root(false);
    }
    SegmentIterator::Iter(iter)
}
