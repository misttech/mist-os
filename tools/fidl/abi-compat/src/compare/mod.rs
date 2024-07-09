// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module is concerned with comparing different platform versions. It
//! contains data structure to represent the platform API in ways that are
//! convenient for comparison, and the comparison algorithms.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Display;

use anyhow::Result;
use flyweights::FlyStr;

use crate::ir::Openness;

mod types;
pub use types::*;
mod handle;
pub use handle::*;
mod path;
pub use path::*;
mod primitive;
pub use primitive::*;
mod problems;
pub use problems::*;

#[cfg(test)]
mod protocol_tests;
#[cfg(test)]
mod test;
#[cfg(test)]
mod types_tests;

#[derive(PartialEq, PartialOrd, Eq, Ord, Debug, Clone, Copy)]
pub enum Optionality {
    Optional,
    Required,
}

#[derive(PartialEq, PartialOrd, Eq, Ord, Debug, Clone, Copy)]
pub enum Flexibility {
    Strict,
    Flexible,
}
impl Display for Flexibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Flexibility::Strict => f.write_str("strict"),
            Flexibility::Flexible => f.write_str("flexible"),
        }
    }
}

#[derive(PartialEq, PartialOrd, Eq, Ord, Debug, Clone, Copy)]
pub enum Transport {
    Channel,
    Driver,
}

impl Display for Transport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Transport::Channel => f.write_str("Channel"),
            Transport::Driver => f.write_str("Driver"),
        }
    }
}

impl From<&Option<String>> for Transport {
    fn from(value: &Option<String>) -> Self {
        match value {
            Some(transport) => transport.into(),
            None => Self::Channel,
        }
    }
}

impl From<&String> for Transport {
    fn from(value: &String) -> Self {
        match value.as_str() {
            "Channel" => Self::Channel,
            "Driver" => Self::Driver,
            _ => panic!("unknown protocol transport {value}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, PartialOrd)]
pub enum MethodPayload {
    TwoWay(Type, Type),
    OneWay(Type),
    Event(Type),
}

#[derive(Clone, Debug, PartialEq, PartialOrd)]
pub struct Method {
    pub name: FlyStr,
    pub path: Path,
    pub flexibility: Flexibility,
    pub payload: MethodPayload,
}

impl Method {
    pub fn two_way(
        name: impl Into<FlyStr>,
        path: Path,
        flexibility: Flexibility,
        request: Type,
        response: Type,
    ) -> Self {
        Self {
            name: name.into(),
            path,
            flexibility,
            payload: MethodPayload::TwoWay(request, response),
        }
    }
    pub fn one_way(
        name: impl Into<FlyStr>,
        path: Path,
        flexibility: Flexibility,
        request: Type,
    ) -> Self {
        Self { name: name.into(), path, flexibility, payload: MethodPayload::OneWay(request) }
    }
    pub fn event(
        name: impl Into<FlyStr>,
        path: Path,
        flexibility: Flexibility,
        payload: Type,
    ) -> Self {
        Self { name: name.into(), path, flexibility, payload: MethodPayload::Event(payload) }
    }
    fn kind(&self) -> &'static str {
        use MethodPayload::*;
        match self.payload {
            TwoWay(_, _) => "two-way",
            OneWay(_) => "one-way",
            Event(_) => "event",
        }
    }

    fn server_initiated(&self) -> bool {
        matches!(self.payload, MethodPayload::Event(_))
    }

    fn client_initiated(&self) -> bool {
        !self.server_initiated()
    }

    fn is_one_way(&self) -> bool {
        matches!(self.payload, MethodPayload::OneWay(_))
    }

    pub fn compatible(client: &Self, server: &Self) -> Result<CompatibilityProblems> {
        let mut problems = CompatibilityProblems::default();
        if client.kind() != server.kind() {
            problems.protocol(
                &client.path,
                &server.path,
                format!(
                    "Interaction kind differs between client(@{}):{} and server(@{}):{}",
                    client.path.api_level(),
                    client.kind(),
                    server.path.api_level(),
                    server.kind()
                ),
            );
            return Ok(problems);
        }
        use MethodPayload::*;
        match (&client.payload, &server.payload) {
            (TwoWay(c_request, c_response), TwoWay(s_request, s_response)) => {
                // Request
                let compat = compare_types(c_request, s_request);
                if compat.is_incompatible() {
                    problems.protocol(
                        &client.path,
                        &server.path,
                        format!(
                            "Incompatible request types, client(@{}), server(@{})",
                            client.path.api_level(),
                            server.path.api_level(),
                        ),
                    );
                }
                problems.append(compat);
                // Response
                let compat = compare_types(s_response, c_response);
                if compat.is_incompatible() {
                    problems.protocol(
                        &client.path,
                        &server.path,
                        format!(
                            "Incompatible response types, client(@{}), server(@{})",
                            client.path.api_level(),
                            server.path.api_level(),
                        ),
                    );
                }
                problems.append(compat);
            }
            (OneWay(c_request), OneWay(s_request)) => {
                let compat = compare_types(c_request, s_request);
                if compat.is_incompatible() {
                    problems.protocol(
                        &client.path,
                        &server.path,
                        format!(
                            "Incompatible request types, client(@{}), server(@{})",
                            client.path.api_level(),
                            server.path.api_level(),
                        ),
                    );
                }
                problems.append(compat);
            }
            (Event(c_payload), Event(s_payload)) => {
                let compat = compare_types(s_payload, c_payload);
                if compat.is_incompatible() {
                    problems.protocol(
                        &client.path,
                        &server.path,
                        format!(
                            "Incompatible event types, client(@{}), server(@{})",
                            client.path.api_level(),
                            server.path.api_level(),
                        ),
                    );
                }
                problems.append(compat);
            }
            _ => (), // Interaction kind mismatch handled elsewhere.
        }

        Ok(problems)
    }
}

#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Debug)]
pub struct ImplementationLocation {
    pub platform: bool,
    pub external: bool,
}

#[derive(Clone, Eq, PartialEq, PartialOrd, Ord, Debug)]
pub struct Discoverable {
    pub name: String,
    pub client: ImplementationLocation,
    pub server: ImplementationLocation,
}

#[derive(Clone, Debug, PartialEq, PartialOrd)]
pub struct Protocol {
    pub name: FlyStr,
    pub path: Path,
    pub openness: Openness,
    pub discoverable: Option<Discoverable>,
    pub methods: BTreeMap<u64, Method>,
    pub added: String,
}

impl Protocol {
    fn compatible(external: &Self, platform: &Self) -> Result<CompatibilityProblems> {
        let mut problems = CompatibilityProblems::default();

        // Look at discoverable to identify the pairs of client/server interactions
        let mut clients = Vec::new();
        let mut servers = Vec::new();
        let external_discoverable =
            external.discoverable.as_ref().expect("Expected value for external.discoverable");
        if external_discoverable.client.external {
            clients.push(external);
        }
        if external_discoverable.server.external {
            servers.push(external);
        }
        let platform_discoverable =
            platform.discoverable.as_ref().expect("Expected value for platform.discoverable");
        if platform_discoverable.client.platform {
            clients.push(platform);
        }
        if platform_discoverable.server.platform {
            servers.push(platform);
        }

        for (client, server) in
            clients.into_iter().flat_map(|c| servers.iter().map(move |s| (c, s)))
        {
            problems.append(Protocol::client_server_compatible(client, server)?);
        }

        Ok(problems)
    }

    fn client_server_compatible(client: &Self, server: &Self) -> Result<CompatibilityProblems> {
        let mut problems = CompatibilityProblems::default();
        let client_ordinals: BTreeSet<u64> = client.methods.keys().cloned().collect();
        let server_ordinals: BTreeSet<u64> = server.methods.keys().cloned().collect();

        // Compare common interactions
        for ordinal in client_ordinals.intersection(&server_ordinals) {
            problems.append(Method::compatible(
                client.methods.get(ordinal).expect("Unexpectedly missing client method"),
                server.methods.get(ordinal).expect("Unexpectedly missing server method"),
            )?);
        }

        // Only on client.
        for ordinal in client_ordinals.difference(&server_ordinals) {
            let method = client.methods.get(ordinal).expect("Unexpectedly missing client method");
            if method.client_initiated() {
                let client_sets_unknown_bit = method.flexibility == Flexibility::Flexible;
                let server_allows_unknown = server.openness == Openness::Open
                    || method.is_one_way() && server.openness == Openness::Ajar;
                if client_sets_unknown_bit && server_allows_unknown {
                    // The server doesn't know about this method but: (a) the
                    // client sets the "unknown" header bit indicating that it's
                    // fine with the server not recognizing it, and (b) the
                    // server is expecting messages with the "unknown" bit set.
                } else {
                    problems.protocol(
                        &method.path,
                        &server.path,
                        format!(
                            "Server(@{}) missing method {}.{}",
                            server.path.api_level(),
                            client.name,
                            method.name
                        ),
                    );
                }
            }
        }

        // Only on server.
        for ordinal in server_ordinals.difference(&client_ordinals) {
            let method = server.methods.get(ordinal).expect("Unexpectedly missing server method");
            if method.server_initiated() {
                if method.flexibility == Flexibility::Flexible
                    && (server.openness == Openness::Open || server.openness == Openness::Ajar)
                {
                    // The server thinks the event is flexible and the client thinks the protocol is open enough.
                } else {
                    problems.protocol(
                        &client.path,
                        &method.path,
                        format!(
                            "Client(@{}) missing event {}.{}",
                            client.path.api_level(),
                            server.name,
                            method.name
                        ),
                    );
                }
            }
        }

        Ok(problems)
    }
}

#[derive(Default, Clone, Debug)]
pub struct Platform {
    pub api_level: FlyStr,
    pub discoverable: HashMap<String, Protocol>,
    pub tear_off: HashMap<String, Protocol>,
}

pub fn compatible(external: &Platform, platform: &Platform) -> Result<CompatibilityProblems> {
    let mut problems = CompatibilityProblems::default();

    let external_discoverable: HashSet<String> = external.discoverable.keys().cloned().collect();
    let platform_discoverable: HashSet<String> = platform.discoverable.keys().cloned().collect();
    for p in external_discoverable.difference(&platform_discoverable) {
        let d = external
            .discoverable
            .get(p)
            .and_then(|p| p.discoverable.as_ref())
            .expect("Expected discoverable attribute");
        if d.server.platform {
            problems.platform(
                &external.discoverable[p].path,
                &Path::new(&platform.api_level),
                format!("Discoverable protocol missing from: platform"),
            );
        }
    }

    for p in external_discoverable.intersection(&platform_discoverable) {
        problems
            .append(Protocol::compatible(&external.discoverable[p], &platform.discoverable[p])?);
    }

    problems.sort();

    Ok(problems)
}
