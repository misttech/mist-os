// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! TODO(https://fxbug.dev/42084621): Types and functions in this file are taken from wlancfg. Dedupe later.

use ieee80211::Bssid;
use log::warn;
use std::cmp::Reverse;
use std::collections::HashSet;
use std::convert::TryFrom;
use wlan_common::scan::Compatible;
use wlan_common::security::wep::WepKey;
use wlan_common::security::wpa::credential::Passphrase;
use wlan_common::security::wpa::WpaDescriptor;
use wlan_common::security::{SecurityAuthenticator, SecurityDescriptor};

#[derive(Clone, Debug, PartialEq)]
pub enum Credential {
    None,
    Password(Vec<u8>),
}

impl Credential {
    pub fn type_str(&self) -> &str {
        match self {
            Credential::None => "None",
            Credential::Password(_) => "Password",
        }
    }
}

pub fn get_authenticator(
    bssid: Bssid,
    compatible: Compatible,
    credential: &Credential,
) -> Option<SecurityAuthenticator> {
    let mutual_security_protocols = compatible.mutual_security_protocols().clone();
    match select_authentication_method(mutual_security_protocols.clone(), credential) {
        Some(authenticator) => Some(authenticator),
        None => {
            warn!(
                "Failed to negotiate authentication for BSS ({:?}) with mutually supported
                security protocols: {:?}, and credential type: {:?}.",
                bssid,
                mutual_security_protocols,
                credential.type_str()
            );
            None
        }
    }
}

/// Binds a credential to a security protocol.
///
/// Binding constructs a `SecurityAuthenticator` that can be used to construct an SME
/// `ConnectRequest`. This function is similar to `SecurityDescriptor::bind`, but operates on the
/// Policy `Credential` type, which requires some additional logic to determine how the credential
/// data is interpreted.
///
/// Returns `None` if the given protocol is incompatible with the given credential.
fn bind_credential_to_protocol(
    protocol: SecurityDescriptor,
    credential: &Credential,
) -> Option<SecurityAuthenticator> {
    match protocol {
        SecurityDescriptor::Open => match credential {
            Credential::None => protocol.bind(None).ok(),
            _ => None,
        },
        SecurityDescriptor::Wep => match credential {
            Credential::Password(ref key) => {
                WepKey::parse(key).ok().and_then(|key| protocol.bind(Some(key.into())).ok())
            }
            _ => None,
        },
        SecurityDescriptor::Wpa(wpa) => match wpa {
            WpaDescriptor::Wpa1 { .. } | WpaDescriptor::Wpa2 { .. } => match credential {
                Credential::Password(ref passphrase) => Passphrase::try_from(passphrase.as_slice())
                    .ok()
                    .and_then(|passphrase| protocol.bind(Some(passphrase.into())).ok()),
                _ => None,
            },
            WpaDescriptor::Wpa3 { .. } => match credential {
                Credential::Password(ref passphrase) => Passphrase::try_from(passphrase.as_slice())
                    .ok()
                    .and_then(|passphrase| protocol.bind(Some(passphrase.into())).ok()),
                _ => None,
            },
        },
    }
}

/// Creates a security authenticator based on supported security protocols and credentials.
///
/// The authentication method is chosen based on the general strength of each mutually supported
/// security protocol (the protocols supported by both the local and remote stations) and the
/// compatibility of those protocols with the given credentials.
///
/// Returns `None` if no appropriate authentication method can be selected for the given protocols
/// and credentials.
pub fn select_authentication_method(
    mutual_security_protocols: HashSet<SecurityDescriptor>,
    credential: &Credential,
) -> Option<SecurityAuthenticator> {
    let mut protocols: Vec<_> = mutual_security_protocols.into_iter().collect();
    protocols.sort_by_key(|protocol| {
        Reverse(match protocol {
            SecurityDescriptor::Open => 0,
            SecurityDescriptor::Wep => 1,
            SecurityDescriptor::Wpa(ref wpa) => match wpa {
                WpaDescriptor::Wpa1 { .. } => 2,
                WpaDescriptor::Wpa2 { .. } => 3,
                WpaDescriptor::Wpa3 { .. } => 4,
            },
        })
    });
    protocols
        .into_iter()
        .flat_map(|protocol| bind_credential_to_protocol(protocol, credential))
        .next()
}
