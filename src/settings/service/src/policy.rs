// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde::{Deserialize, Serialize};
use settings_storage::device_storage::DeviceStorageCompatible;
use std::convert::TryFrom;

/// The policy types supported by the service.
#[derive(PartialEq, Debug, Eq, Hash, Clone, Copy, Serialize, Deserialize)]
pub enum PolicyType {
    /// This type is reserved for testing purposes.
    Unknown,
}

pub(crate) trait HasPolicyType {
    const POLICY_TYPE: PolicyType;
}

/// Enumeration over the possible policy state information for all policies.
#[derive(PartialEq, Debug, Clone)]
#[allow(dead_code)]
pub enum PolicyInfo {
    /// This value is reserved for testing purposes.
    Unknown(UnknownInfo),
}

macro_rules! conversion_impls {
    ($($(#[cfg($test:meta)])? $variant:ident($info_ty:ty) => $ty_variant:ident ),+ $(,)?) => {
        $(
            $(#[cfg($test)])?
            impl HasPolicyType for $info_ty {
                const POLICY_TYPE: PolicyType = PolicyType::$ty_variant;
            }

            $(#[cfg($test)])?
            impl TryFrom<PolicyInfo> for $info_ty {
                type Error = ();

                fn try_from(setting_info: PolicyInfo) -> Result<Self, ()> {
                    // Remove allow once additional non-test variant is added.
                    #[allow(unreachable_patterns)]
                    match setting_info {
                        PolicyInfo::$variant(info) => Ok(info),
                        _ => Err(()),
                    }
                }
            }
        )+
    }
}

conversion_impls! {
    Unknown(UnknownInfo) => Unknown,
}

impl DeviceStorageCompatible for UnknownInfo {
    const KEY: &'static str = "unknown_info";

    fn default_value() -> Self {
        Self(false)
    }
}

impl From<UnknownInfo> for PolicyInfo {
    fn from(unknown_info: UnknownInfo) -> Self {
        PolicyInfo::Unknown(unknown_info)
    }
}

impl From<&PolicyInfo> for PolicyType {
    fn from(policy_info: &PolicyInfo) -> Self {
        match policy_info {
            PolicyInfo::Unknown(_) => PolicyType::Unknown,
        }
    }
}

/// This struct is reserved for testing purposes.
#[derive(PartialEq, Debug, Copy, Clone, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct UnknownInfo(pub bool);

/// `Role` defines grouping for responsibilities on the policy message hub.
#[derive(PartialEq, Copy, Clone, Debug, Eq, Hash)]
pub enum Role {
    /// This role indicates that the messenger handles and enacts policy requests.
    PolicyHandler,
}
