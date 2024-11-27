// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Extensions for the fuchsia.net.filter FIDL library.
//!
//! Note that this library as written is not meant for inclusion in the SDK. It
//! is only meant to be used in conjunction with a netstack that is compiled
//! against the same API level of the `fuchsia.net.filter` FIDL library. This
//! library opts in to compile-time and runtime breakage when the FIDL library
//! is evolved in order to enforce that it is updated along with the FIDL
//! library itself.

#[cfg(target_os = "fuchsia")]
pub mod sync;

use std::collections::HashMap;
use std::fmt::Debug;
use std::num::{NonZeroU16, NonZeroU64};
use std::ops::RangeInclusive;

use async_utils::fold::FoldWhile;
use fidl::marker::SourceBreaking;
use fidl_fuchsia_net_ext::IntoExt;
use futures::{Stream, StreamExt as _, TryStreamExt as _};
use thiserror::Error;
use {
    fidl_fuchsia_net as fnet, fidl_fuchsia_net_filter as fnet_filter,
    fidl_fuchsia_net_interfaces as fnet_interfaces,
    fidl_fuchsia_net_interfaces_ext as fnet_interfaces_ext, fidl_fuchsia_net_root as fnet_root,
};

/// Conversion errors from `fnet_filter` FIDL types to the
/// equivalents defined in this module.
#[derive(Debug, Error, PartialEq)]
pub enum FidlConversionError {
    #[error("union is of an unknown variant: {0}")]
    UnknownUnionVariant(&'static str),
    #[error("namespace ID not provided")]
    MissingNamespaceId,
    #[error("namespace domain not provided")]
    MissingNamespaceDomain,
    #[error("routine ID not provided")]
    MissingRoutineId,
    #[error("routine type not provided")]
    MissingRoutineType,
    #[error("IP installation hook not provided")]
    MissingIpInstallationHook,
    #[error("NAT installation hook not provided")]
    MissingNatInstallationHook,
    #[error("interface matcher specified an invalid ID of 0")]
    ZeroInterfaceId,
    #[error("invalid address range (start must be <= end)")]
    InvalidAddressRange,
    #[error("address range start and end addresses are not the same IP family")]
    AddressRangeFamilyMismatch,
    #[error("prefix length of subnet is longer than number of bits in IP address")]
    SubnetPrefixTooLong,
    #[error("host bits are set in subnet network")]
    SubnetHostBitsSet,
    #[error("invalid port matcher range (start must be <= end)")]
    InvalidPortMatcherRange,
    #[error("transparent proxy action specified an invalid local port of 0")]
    UnspecifiedTransparentProxyPort,
    #[error("NAT action specified an invalid rewrite port of 0")]
    UnspecifiedNatPort,
    #[error("invalid port range (start must be <= end)")]
    InvalidPortRange,
    #[error("non-error result variant could not be converted to an error")]
    NotAnError,
}

// TODO(https://fxbug.dev/317058051): remove this when the Rust FIDL bindings
// expose constants for these.
mod type_names {
    pub(super) const RESOURCE_ID: &str = "fuchsia.net.filter/ResourceId";
    pub(super) const DOMAIN: &str = "fuchsia.net.filter/Domain";
    pub(super) const IP_INSTALLATION_HOOK: &str = "fuchsia.net.filter/IpInstallationHook";
    pub(super) const NAT_INSTALLATION_HOOK: &str = "fuchsia.net.filter/NatInstallationHook";
    pub(super) const ROUTINE_TYPE: &str = "fuchsia.net.filter/RoutineType";
    pub(super) const INTERFACE_MATCHER: &str = "fuchsia.net.filter/InterfaceMatcher";
    pub(super) const ADDRESS_MATCHER_TYPE: &str = "fuchsia.net.filter/AddressMatcherType";
    pub(super) const TRANSPORT_PROTOCOL: &str = "fuchsia.net.filter/TransportProtocol";
    pub(super) const ACTION: &str = "fuchsia.net.filter/Action";
    pub(super) const TRANSPARENT_PROXY: &str = "fuchsia.net.filter/TransparentProxy";
    pub(super) const RESOURCE: &str = "fuchsia.net.filter/Resource";
    pub(super) const EVENT: &str = "fuchsia.net.filter/Event";
    pub(super) const CHANGE: &str = "fuchsia.net.filter/Change";
    pub(super) const CHANGE_VALIDATION_ERROR: &str = "fuchsia.net.filter/ChangeValidationError";
    pub(super) const CHANGE_VALIDATION_RESULT: &str = "fuchsia.net.filter/ChangeValidationResult";
    pub(super) const COMMIT_ERROR: &str = "fuchsia.net.filter/CommitError";
    pub(super) const COMMIT_RESULT: &str = "fuchsia.net.filter/CommitResult";
    pub(super) const NET_INTERFACES_PORT_CLASS: &str = "fuchsia.net.interfaces/PortClass";
    pub(super) const HARDWARE_NETWORK_PORT_CLASS: &str = "fuchsia.hardware.network/PortClass";
}

/// Extension type for [`fnet_filter::NamespaceId`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct NamespaceId(pub String);

/// Extension type for [`fnet_filter::RoutineId`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RoutineId {
    pub namespace: NamespaceId,
    pub name: String,
}

impl From<fnet_filter::RoutineId> for RoutineId {
    fn from(id: fnet_filter::RoutineId) -> Self {
        let fnet_filter::RoutineId { namespace, name } = id;
        Self { namespace: NamespaceId(namespace), name }
    }
}

impl From<RoutineId> for fnet_filter::RoutineId {
    fn from(id: RoutineId) -> Self {
        let RoutineId { namespace, name } = id;
        let NamespaceId(namespace) = namespace;
        Self { namespace, name }
    }
}

/// Extension type for [`fnet_filter::RuleId`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RuleId {
    pub routine: RoutineId,
    pub index: u32,
}

impl From<fnet_filter::RuleId> for RuleId {
    fn from(id: fnet_filter::RuleId) -> Self {
        let fnet_filter::RuleId { routine, index } = id;
        Self { routine: routine.into(), index }
    }
}

impl From<RuleId> for fnet_filter::RuleId {
    fn from(id: RuleId) -> Self {
        let RuleId { routine, index } = id;
        Self { routine: routine.into(), index }
    }
}

/// Extension type for [`fnet_filter::ResourceId`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ResourceId {
    Namespace(NamespaceId),
    Routine(RoutineId),
    Rule(RuleId),
}

impl TryFrom<fnet_filter::ResourceId> for ResourceId {
    type Error = FidlConversionError;

    fn try_from(id: fnet_filter::ResourceId) -> Result<Self, Self::Error> {
        match id {
            fnet_filter::ResourceId::Namespace(id) => Ok(Self::Namespace(NamespaceId(id))),
            fnet_filter::ResourceId::Routine(id) => Ok(Self::Routine(id.into())),
            fnet_filter::ResourceId::Rule(id) => Ok(Self::Rule(id.into())),
            fnet_filter::ResourceId::__SourceBreaking { .. } => {
                Err(FidlConversionError::UnknownUnionVariant(type_names::RESOURCE_ID))
            }
        }
    }
}

impl From<ResourceId> for fnet_filter::ResourceId {
    fn from(id: ResourceId) -> Self {
        match id {
            ResourceId::Namespace(NamespaceId(id)) => fnet_filter::ResourceId::Namespace(id),
            ResourceId::Routine(id) => fnet_filter::ResourceId::Routine(id.into()),
            ResourceId::Rule(id) => fnet_filter::ResourceId::Rule(id.into()),
        }
    }
}

/// Extension type for [`fnet_filter::Domain`].
#[derive(Debug, Clone, PartialEq)]
pub enum Domain {
    Ipv4,
    Ipv6,
    AllIp,
}

impl From<Domain> for fnet_filter::Domain {
    fn from(domain: Domain) -> Self {
        match domain {
            Domain::Ipv4 => fnet_filter::Domain::Ipv4,
            Domain::Ipv6 => fnet_filter::Domain::Ipv6,
            Domain::AllIp => fnet_filter::Domain::AllIp,
        }
    }
}

impl TryFrom<fnet_filter::Domain> for Domain {
    type Error = FidlConversionError;

    fn try_from(domain: fnet_filter::Domain) -> Result<Self, Self::Error> {
        match domain {
            fnet_filter::Domain::Ipv4 => Ok(Self::Ipv4),
            fnet_filter::Domain::Ipv6 => Ok(Self::Ipv6),
            fnet_filter::Domain::AllIp => Ok(Self::AllIp),
            fnet_filter::Domain::__SourceBreaking { .. } => {
                Err(FidlConversionError::UnknownUnionVariant(type_names::DOMAIN))
            }
        }
    }
}

/// Extension type for [`fnet_filter::Namespace`].
#[derive(Debug, Clone, PartialEq)]
pub struct Namespace {
    pub id: NamespaceId,
    pub domain: Domain,
}

impl From<Namespace> for fnet_filter::Namespace {
    fn from(namespace: Namespace) -> Self {
        let Namespace { id, domain } = namespace;
        let NamespaceId(id) = id;
        Self { id: Some(id), domain: Some(domain.into()), __source_breaking: SourceBreaking }
    }
}

impl TryFrom<fnet_filter::Namespace> for Namespace {
    type Error = FidlConversionError;

    fn try_from(namespace: fnet_filter::Namespace) -> Result<Self, Self::Error> {
        let fnet_filter::Namespace { id, domain, __source_breaking } = namespace;
        let id = NamespaceId(id.ok_or(FidlConversionError::MissingNamespaceId)?);
        let domain = domain.ok_or(FidlConversionError::MissingNamespaceDomain)?.try_into()?;
        Ok(Self { id, domain })
    }
}

/// Extension type for [`fnet_filter::IpInstallationHook`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IpHook {
    Ingress,
    LocalIngress,
    Forwarding,
    LocalEgress,
    Egress,
}

impl From<IpHook> for fnet_filter::IpInstallationHook {
    fn from(hook: IpHook) -> Self {
        match hook {
            IpHook::Ingress => Self::Ingress,
            IpHook::LocalIngress => Self::LocalIngress,
            IpHook::Forwarding => Self::Forwarding,
            IpHook::LocalEgress => Self::LocalEgress,
            IpHook::Egress => Self::Egress,
        }
    }
}

impl TryFrom<fnet_filter::IpInstallationHook> for IpHook {
    type Error = FidlConversionError;

    fn try_from(hook: fnet_filter::IpInstallationHook) -> Result<Self, Self::Error> {
        match hook {
            fnet_filter::IpInstallationHook::Ingress => Ok(Self::Ingress),
            fnet_filter::IpInstallationHook::LocalIngress => Ok(Self::LocalIngress),
            fnet_filter::IpInstallationHook::Forwarding => Ok(Self::Forwarding),
            fnet_filter::IpInstallationHook::LocalEgress => Ok(Self::LocalEgress),
            fnet_filter::IpInstallationHook::Egress => Ok(Self::Egress),
            fnet_filter::IpInstallationHook::__SourceBreaking { .. } => {
                Err(FidlConversionError::UnknownUnionVariant(type_names::IP_INSTALLATION_HOOK))
            }
        }
    }
}

/// Extension type for [`fnet_filter::NatInstallationHook`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NatHook {
    Ingress,
    LocalIngress,
    LocalEgress,
    Egress,
}

impl From<NatHook> for fnet_filter::NatInstallationHook {
    fn from(hook: NatHook) -> Self {
        match hook {
            NatHook::Ingress => Self::Ingress,
            NatHook::LocalIngress => Self::LocalIngress,
            NatHook::LocalEgress => Self::LocalEgress,
            NatHook::Egress => Self::Egress,
        }
    }
}

impl TryFrom<fnet_filter::NatInstallationHook> for NatHook {
    type Error = FidlConversionError;

    fn try_from(hook: fnet_filter::NatInstallationHook) -> Result<Self, Self::Error> {
        match hook {
            fnet_filter::NatInstallationHook::Ingress => Ok(Self::Ingress),
            fnet_filter::NatInstallationHook::LocalIngress => Ok(Self::LocalIngress),
            fnet_filter::NatInstallationHook::LocalEgress => Ok(Self::LocalEgress),
            fnet_filter::NatInstallationHook::Egress => Ok(Self::Egress),
            fnet_filter::NatInstallationHook::__SourceBreaking { .. } => {
                Err(FidlConversionError::UnknownUnionVariant(type_names::NAT_INSTALLATION_HOOK))
            }
        }
    }
}

/// Extension type for [`fnet_filter::InstalledIpRoutine`].
#[derive(Debug, Clone, PartialEq)]
pub struct InstalledIpRoutine {
    pub hook: IpHook,
    pub priority: i32,
}

impl From<InstalledIpRoutine> for fnet_filter::InstalledIpRoutine {
    fn from(routine: InstalledIpRoutine) -> Self {
        let InstalledIpRoutine { hook, priority } = routine;
        Self {
            hook: Some(hook.into()),
            priority: Some(priority),
            __source_breaking: SourceBreaking,
        }
    }
}

impl TryFrom<fnet_filter::InstalledIpRoutine> for InstalledIpRoutine {
    type Error = FidlConversionError;

    fn try_from(routine: fnet_filter::InstalledIpRoutine) -> Result<Self, Self::Error> {
        let fnet_filter::InstalledIpRoutine { hook, priority, __source_breaking } = routine;
        let hook = hook.ok_or(FidlConversionError::MissingIpInstallationHook)?;
        let priority = priority.unwrap_or(fnet_filter::DEFAULT_ROUTINE_PRIORITY);
        Ok(Self { hook: hook.try_into()?, priority })
    }
}

/// Extension type for [`fnet_filter::InstalledNatRoutine`].
#[derive(Debug, Clone, PartialEq)]
pub struct InstalledNatRoutine {
    pub hook: NatHook,
    pub priority: i32,
}

impl From<InstalledNatRoutine> for fnet_filter::InstalledNatRoutine {
    fn from(routine: InstalledNatRoutine) -> Self {
        let InstalledNatRoutine { hook, priority } = routine;
        Self {
            hook: Some(hook.into()),
            priority: Some(priority),
            __source_breaking: SourceBreaking,
        }
    }
}

impl TryFrom<fnet_filter::InstalledNatRoutine> for InstalledNatRoutine {
    type Error = FidlConversionError;

    fn try_from(routine: fnet_filter::InstalledNatRoutine) -> Result<Self, Self::Error> {
        let fnet_filter::InstalledNatRoutine { hook, priority, __source_breaking } = routine;
        let hook = hook.ok_or(FidlConversionError::MissingNatInstallationHook)?;
        let priority = priority.unwrap_or(fnet_filter::DEFAULT_ROUTINE_PRIORITY);
        Ok(Self { hook: hook.try_into()?, priority })
    }
}

/// Extension type for [`fnet_filter::RoutineType`].
#[derive(Debug, Clone, PartialEq)]
pub enum RoutineType {
    Ip(Option<InstalledIpRoutine>),
    Nat(Option<InstalledNatRoutine>),
}

impl RoutineType {
    pub fn is_installed(&self) -> bool {
        // The `InstalledIpRoutine` or `InstalledNatRoutine` configuration is
        // optional, and when omitted, signifies an uninstalled routine.
        match self {
            Self::Ip(Some(_)) | Self::Nat(Some(_)) => true,
            Self::Ip(None) | Self::Nat(None) => false,
        }
    }
}

impl From<RoutineType> for fnet_filter::RoutineType {
    fn from(routine: RoutineType) -> Self {
        match routine {
            RoutineType::Ip(installation) => Self::Ip(fnet_filter::IpRoutine {
                installation: installation.map(Into::into),
                __source_breaking: SourceBreaking,
            }),
            RoutineType::Nat(installation) => Self::Nat(fnet_filter::NatRoutine {
                installation: installation.map(Into::into),
                __source_breaking: SourceBreaking,
            }),
        }
    }
}

impl TryFrom<fnet_filter::RoutineType> for RoutineType {
    type Error = FidlConversionError;

    fn try_from(type_: fnet_filter::RoutineType) -> Result<Self, Self::Error> {
        match type_ {
            fnet_filter::RoutineType::Ip(fnet_filter::IpRoutine {
                installation,
                __source_breaking,
            }) => Ok(RoutineType::Ip(installation.map(TryInto::try_into).transpose()?)),
            fnet_filter::RoutineType::Nat(fnet_filter::NatRoutine {
                installation,
                __source_breaking,
            }) => Ok(RoutineType::Nat(installation.map(TryInto::try_into).transpose()?)),
            fnet_filter::RoutineType::__SourceBreaking { .. } => {
                Err(FidlConversionError::UnknownUnionVariant(type_names::ROUTINE_TYPE))
            }
        }
    }
}

/// Extension type for [`fnet_filter::Routine`].
#[derive(Debug, Clone, PartialEq)]
pub struct Routine {
    pub id: RoutineId,
    pub routine_type: RoutineType,
}

impl From<Routine> for fnet_filter::Routine {
    fn from(routine: Routine) -> Self {
        let Routine { id, routine_type: type_ } = routine;
        Self { id: Some(id.into()), type_: Some(type_.into()), __source_breaking: SourceBreaking }
    }
}

impl TryFrom<fnet_filter::Routine> for Routine {
    type Error = FidlConversionError;

    fn try_from(routine: fnet_filter::Routine) -> Result<Self, Self::Error> {
        let fnet_filter::Routine { id, type_, __source_breaking } = routine;
        let id = id.ok_or(FidlConversionError::MissingRoutineId)?;
        let type_ = type_.ok_or(FidlConversionError::MissingRoutineType)?;
        Ok(Self { id: id.into(), routine_type: type_.try_into()? })
    }
}

/// Extension type for [`fnet_filter::InterfaceMatcher`].
#[derive(Debug, Clone, PartialEq)]
pub enum InterfaceMatcher {
    Id(NonZeroU64),
    Name(fnet_interfaces::Name),
    PortClass(fnet_interfaces_ext::PortClass),
}

impl From<InterfaceMatcher> for fnet_filter::InterfaceMatcher {
    fn from(matcher: InterfaceMatcher) -> Self {
        match matcher {
            InterfaceMatcher::Id(id) => Self::Id(id.get()),
            InterfaceMatcher::Name(name) => Self::Name(name),
            InterfaceMatcher::PortClass(port_class) => Self::PortClass(port_class.into()),
        }
    }
}

impl TryFrom<fnet_filter::InterfaceMatcher> for InterfaceMatcher {
    type Error = FidlConversionError;

    fn try_from(matcher: fnet_filter::InterfaceMatcher) -> Result<Self, Self::Error> {
        match matcher {
            fnet_filter::InterfaceMatcher::Id(id) => {
                let id = NonZeroU64::new(id).ok_or(FidlConversionError::ZeroInterfaceId)?;
                Ok(Self::Id(id))
            }
            fnet_filter::InterfaceMatcher::Name(name) => Ok(Self::Name(name)),
            fnet_filter::InterfaceMatcher::PortClass(port_class) => {
                port_class.try_into().map(Self::PortClass).map_err(|e| match e {
                    fnet_interfaces_ext::UnknownPortClassError::NetInterfaces(_) => {
                        FidlConversionError::UnknownUnionVariant(
                            type_names::NET_INTERFACES_PORT_CLASS,
                        )
                    }
                    fnet_interfaces_ext::UnknownPortClassError::HardwareNetwork(_) => {
                        FidlConversionError::UnknownUnionVariant(
                            type_names::HARDWARE_NETWORK_PORT_CLASS,
                        )
                    }
                })
            }
            fnet_filter::InterfaceMatcher::__SourceBreaking { .. } => {
                Err(FidlConversionError::UnknownUnionVariant(type_names::INTERFACE_MATCHER))
            }
        }
    }
}

/// Extension type for the `Subnet` variant of [`fnet_filter::AddressMatcherType`].
///
/// This type witnesses to the invariant that the prefix length of the subnet is
/// no greater than the number of bits in the IP address, and that no host bits
/// in the network address are set.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct Subnet(fnet::Subnet);

impl Subnet {
    pub fn get(&self) -> fnet::Subnet {
        let Subnet(subnet) = &self;
        *subnet
    }
}

impl From<Subnet> for fnet::Subnet {
    fn from(subnet: Subnet) -> Self {
        let Subnet(subnet) = subnet;
        subnet
    }
}

impl TryFrom<fnet::Subnet> for Subnet {
    type Error = FidlConversionError;

    fn try_from(subnet: fnet::Subnet) -> Result<Self, Self::Error> {
        let fnet::Subnet { addr, prefix_len } = subnet;

        // We convert to `net_types::ip::Subnet` to validate the subnet's
        // properties, but we don't store the subnet as that type because we
        // want to avoid forcing all `Resource` types in this library to be
        // parameterized on IP version.
        let result = match addr {
            fnet::IpAddress::Ipv4(v4) => {
                net_types::ip::Subnet::<net_types::ip::Ipv4Addr>::new(v4.into_ext(), prefix_len)
                    .map(|_| Subnet(subnet))
            }
            fnet::IpAddress::Ipv6(v6) => {
                net_types::ip::Subnet::<net_types::ip::Ipv6Addr>::new(v6.into_ext(), prefix_len)
                    .map(|_| Subnet(subnet))
            }
        };
        result.map_err(|e| match e {
            net_types::ip::SubnetError::PrefixTooLong => FidlConversionError::SubnetPrefixTooLong,
            net_types::ip::SubnetError::HostBitsSet => FidlConversionError::SubnetHostBitsSet,
        })
    }
}

impl Debug for Subnet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let fnet::Subnet { addr, prefix_len } = self.0;

        match addr {
            fnet::IpAddress::Ipv4(v4) => {
                let subnet = net_types::ip::Subnet::<net_types::ip::Ipv4Addr>::new(
                    v4.into_ext(),
                    prefix_len,
                );

                match subnet {
                    Ok(inner) => inner.fmt(f),
                    Err(err) => err.fmt(f),
                }
            }
            fnet::IpAddress::Ipv6(v6) => {
                let subnet = net_types::ip::Subnet::<net_types::ip::Ipv6Addr>::new(
                    v6.into_ext(),
                    prefix_len,
                );

                match subnet {
                    Ok(inner) => inner.fmt(f),
                    Err(err) => err.fmt(f),
                }
            }
        }
    }
}

/// Extension type for [`fnet_filter::AddressRange`].
///
/// This type witnesses to the invariant that `start` is in the same IP family
/// as `end`, and that `start <= end`. (Comparisons are performed on the
/// numerical big-endian representation of the IP address.)
#[derive(Debug, Clone, PartialEq)]
pub struct AddressRange {
    range: RangeInclusive<fnet::IpAddress>,
}

impl AddressRange {
    pub fn start(&self) -> fnet::IpAddress {
        *self.range.start()
    }

    pub fn end(&self) -> fnet::IpAddress {
        *self.range.end()
    }
}

impl From<AddressRange> for fnet_filter::AddressRange {
    fn from(range: AddressRange) -> Self {
        Self { start: range.start(), end: range.end() }
    }
}

impl TryFrom<fnet_filter::AddressRange> for AddressRange {
    type Error = FidlConversionError;

    fn try_from(range: fnet_filter::AddressRange) -> Result<Self, Self::Error> {
        let fnet_filter::AddressRange { start, end } = range;
        match (start, end) {
            (
                fnet::IpAddress::Ipv4(fnet::Ipv4Address { addr: start_bytes }),
                fnet::IpAddress::Ipv4(fnet::Ipv4Address { addr: end_bytes }),
            ) => {
                if u32::from_be_bytes(start_bytes) > u32::from_be_bytes(end_bytes) {
                    Err(FidlConversionError::InvalidAddressRange)
                } else {
                    Ok(Self { range: start..=end })
                }
            }
            (
                fnet::IpAddress::Ipv6(fnet::Ipv6Address { addr: start_bytes }),
                fnet::IpAddress::Ipv6(fnet::Ipv6Address { addr: end_bytes }),
            ) => {
                if u128::from_be_bytes(start_bytes) > u128::from_be_bytes(end_bytes) {
                    Err(FidlConversionError::InvalidAddressRange)
                } else {
                    Ok(Self { range: start..=end })
                }
            }
            _ => Err(FidlConversionError::AddressRangeFamilyMismatch),
        }
    }
}

/// Extension type for [`fnet_filter::AddressMatcherType`].
#[derive(Clone, PartialEq)]
pub enum AddressMatcherType {
    Subnet(Subnet),
    Range(AddressRange),
}

impl From<AddressMatcherType> for fnet_filter::AddressMatcherType {
    fn from(matcher: AddressMatcherType) -> Self {
        match matcher {
            AddressMatcherType::Subnet(subnet) => Self::Subnet(subnet.into()),
            AddressMatcherType::Range(range) => Self::Range(range.into()),
        }
    }
}

impl TryFrom<fnet_filter::AddressMatcherType> for AddressMatcherType {
    type Error = FidlConversionError;

    fn try_from(matcher: fnet_filter::AddressMatcherType) -> Result<Self, Self::Error> {
        match matcher {
            fnet_filter::AddressMatcherType::Subnet(subnet) => Ok(Self::Subnet(subnet.try_into()?)),
            fnet_filter::AddressMatcherType::Range(range) => Ok(Self::Range(range.try_into()?)),
            fnet_filter::AddressMatcherType::__SourceBreaking { .. } => {
                Err(FidlConversionError::UnknownUnionVariant(type_names::ADDRESS_MATCHER_TYPE))
            }
        }
    }
}

impl Debug for AddressMatcherType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AddressMatcherType::Subnet(subnet) => subnet.fmt(f),
            AddressMatcherType::Range(address_range) => address_range.fmt(f),
        }
    }
}

/// Extension type for [`fnet_filter::AddressMatcher`].
#[derive(Debug, Clone, PartialEq)]
pub struct AddressMatcher {
    pub matcher: AddressMatcherType,
    pub invert: bool,
}

impl From<AddressMatcher> for fnet_filter::AddressMatcher {
    fn from(matcher: AddressMatcher) -> Self {
        let AddressMatcher { matcher, invert } = matcher;
        Self { matcher: matcher.into(), invert }
    }
}

impl TryFrom<fnet_filter::AddressMatcher> for AddressMatcher {
    type Error = FidlConversionError;

    fn try_from(matcher: fnet_filter::AddressMatcher) -> Result<Self, Self::Error> {
        let fnet_filter::AddressMatcher { matcher, invert } = matcher;
        Ok(Self { matcher: matcher.try_into()?, invert })
    }
}

/// Extension type for [`fnet_filter::PortMatcher`].
///
/// This type witnesses to the invariant that `start <= end`.
#[derive(Debug, Clone, PartialEq)]
pub struct PortMatcher {
    range: RangeInclusive<u16>,
    pub invert: bool,
}

/// Errors when creating a `PortMatcher`.
#[derive(Debug, Error, PartialEq)]
pub enum PortMatcherError {
    #[error("invalid port range (start must be <= end)")]
    InvalidPortRange,
}

impl PortMatcher {
    pub fn new(start: u16, end: u16, invert: bool) -> Result<Self, PortMatcherError> {
        if start > end {
            return Err(PortMatcherError::InvalidPortRange);
        }
        Ok(Self { range: start..=end, invert })
    }

    pub fn range(&self) -> &RangeInclusive<u16> {
        &self.range
    }

    pub fn start(&self) -> u16 {
        *self.range.start()
    }

    pub fn end(&self) -> u16 {
        *self.range.end()
    }
}

impl From<PortMatcher> for fnet_filter::PortMatcher {
    fn from(matcher: PortMatcher) -> Self {
        let PortMatcher { range, invert } = matcher;
        Self { start: *range.start(), end: *range.end(), invert }
    }
}

impl TryFrom<fnet_filter::PortMatcher> for PortMatcher {
    type Error = FidlConversionError;

    fn try_from(matcher: fnet_filter::PortMatcher) -> Result<Self, Self::Error> {
        let fnet_filter::PortMatcher { start, end, invert } = matcher;
        if start > end {
            return Err(FidlConversionError::InvalidPortMatcherRange);
        }
        Ok(Self { range: start..=end, invert })
    }
}

/// Extension type for [`fnet_filter::TransportProtocol`].
#[derive(Clone, PartialEq)]
pub enum TransportProtocolMatcher {
    Tcp { src_port: Option<PortMatcher>, dst_port: Option<PortMatcher> },
    Udp { src_port: Option<PortMatcher>, dst_port: Option<PortMatcher> },
    Icmp,
    Icmpv6,
}

impl From<TransportProtocolMatcher> for fnet_filter::TransportProtocol {
    fn from(matcher: TransportProtocolMatcher) -> Self {
        match matcher {
            TransportProtocolMatcher::Tcp { src_port, dst_port } => {
                Self::Tcp(fnet_filter::TcpMatcher {
                    src_port: src_port.map(Into::into),
                    dst_port: dst_port.map(Into::into),
                    __source_breaking: SourceBreaking,
                })
            }
            TransportProtocolMatcher::Udp { src_port, dst_port } => {
                Self::Udp(fnet_filter::UdpMatcher {
                    src_port: src_port.map(Into::into),
                    dst_port: dst_port.map(Into::into),
                    __source_breaking: SourceBreaking,
                })
            }
            TransportProtocolMatcher::Icmp => Self::Icmp(fnet_filter::IcmpMatcher::default()),
            TransportProtocolMatcher::Icmpv6 => Self::Icmpv6(fnet_filter::Icmpv6Matcher::default()),
        }
    }
}

impl TryFrom<fnet_filter::TransportProtocol> for TransportProtocolMatcher {
    type Error = FidlConversionError;

    fn try_from(matcher: fnet_filter::TransportProtocol) -> Result<Self, Self::Error> {
        match matcher {
            fnet_filter::TransportProtocol::Tcp(fnet_filter::TcpMatcher {
                src_port,
                dst_port,
                __source_breaking,
            }) => Ok(Self::Tcp {
                src_port: src_port.map(TryInto::try_into).transpose()?,
                dst_port: dst_port.map(TryInto::try_into).transpose()?,
            }),
            fnet_filter::TransportProtocol::Udp(fnet_filter::UdpMatcher {
                src_port,
                dst_port,
                __source_breaking,
            }) => Ok(Self::Udp {
                src_port: src_port.map(TryInto::try_into).transpose()?,
                dst_port: dst_port.map(TryInto::try_into).transpose()?,
            }),
            fnet_filter::TransportProtocol::Icmp(fnet_filter::IcmpMatcher {
                __source_breaking,
            }) => Ok(Self::Icmp),
            fnet_filter::TransportProtocol::Icmpv6(fnet_filter::Icmpv6Matcher {
                __source_breaking,
            }) => Ok(Self::Icmpv6),
            fnet_filter::TransportProtocol::__SourceBreaking { .. } => {
                Err(FidlConversionError::UnknownUnionVariant(type_names::TRANSPORT_PROTOCOL))
            }
        }
    }
}

impl Debug for TransportProtocolMatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Omit empty fields.
        match self {
            TransportProtocolMatcher::Tcp { src_port, dst_port } => {
                let mut debug_struct = f.debug_struct("Tcp");

                // Omit empty fields.
                if let Some(port) = &src_port {
                    let _ = debug_struct.field("src_port", port);
                }

                if let Some(port) = &dst_port {
                    let _ = debug_struct.field("dst_port", port);
                }

                debug_struct.finish()
            }
            TransportProtocolMatcher::Udp { src_port, dst_port } => {
                let mut debug_struct = f.debug_struct("Udp");

                // Omit empty fields.
                if let Some(port) = &src_port {
                    let _ = debug_struct.field("src_port", port);
                }

                if let Some(port) = &dst_port {
                    let _ = debug_struct.field("dst_port", port);
                }

                debug_struct.finish()
            }
            TransportProtocolMatcher::Icmp => f.write_str("Icmp"),
            TransportProtocolMatcher::Icmpv6 => f.write_str("Icmpv6"),
        }
    }
}

/// Extension type for [`fnet_filter::Matchers`].
#[derive(Default, Clone, PartialEq)]
pub struct Matchers {
    pub in_interface: Option<InterfaceMatcher>,
    pub out_interface: Option<InterfaceMatcher>,
    pub src_addr: Option<AddressMatcher>,
    pub dst_addr: Option<AddressMatcher>,
    pub transport_protocol: Option<TransportProtocolMatcher>,
}

impl From<Matchers> for fnet_filter::Matchers {
    fn from(matchers: Matchers) -> Self {
        let Matchers { in_interface, out_interface, src_addr, dst_addr, transport_protocol } =
            matchers;
        Self {
            in_interface: in_interface.map(Into::into),
            out_interface: out_interface.map(Into::into),
            src_addr: src_addr.map(Into::into),
            dst_addr: dst_addr.map(Into::into),
            transport_protocol: transport_protocol.map(Into::into),
            __source_breaking: SourceBreaking,
        }
    }
}

impl TryFrom<fnet_filter::Matchers> for Matchers {
    type Error = FidlConversionError;

    fn try_from(matchers: fnet_filter::Matchers) -> Result<Self, Self::Error> {
        let fnet_filter::Matchers {
            in_interface,
            out_interface,
            src_addr,
            dst_addr,
            transport_protocol,
            __source_breaking,
        } = matchers;
        Ok(Self {
            in_interface: in_interface.map(TryInto::try_into).transpose()?,
            out_interface: out_interface.map(TryInto::try_into).transpose()?,
            src_addr: src_addr.map(TryInto::try_into).transpose()?,
            dst_addr: dst_addr.map(TryInto::try_into).transpose()?,
            transport_protocol: transport_protocol.map(TryInto::try_into).transpose()?,
        })
    }
}

impl Debug for Matchers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug_struct = f.debug_struct("Matchers");

        let Matchers { in_interface, out_interface, src_addr, dst_addr, transport_protocol } =
            &self;

        // Omit empty fields.
        if let Some(matcher) = in_interface {
            let _ = debug_struct.field("in_interface", matcher);
        }

        if let Some(matcher) = out_interface {
            let _ = debug_struct.field("out_interface", matcher);
        }

        if let Some(matcher) = src_addr {
            let _ = debug_struct.field("src_addr", matcher);
        }

        if let Some(matcher) = dst_addr {
            let _ = debug_struct.field("dst_addr", matcher);
        }

        if let Some(matcher) = transport_protocol {
            let _ = debug_struct.field("transport_protocol", matcher);
        }

        debug_struct.finish()
    }
}

/// Extension type for [`fnet_filter::Action`].
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Accept,
    Drop,
    Jump(String),
    Return,
    TransparentProxy(TransparentProxy),
    Redirect { dst_port: Option<PortRange> },
    Masquerade { src_port: Option<PortRange> },
}

/// Extension type for [`fnet_filter::TransparentProxy_`].
#[derive(Debug, Clone, PartialEq)]
pub enum TransparentProxy {
    LocalAddr(fnet::IpAddress),
    LocalPort(NonZeroU16),
    LocalAddrAndPort(fnet::IpAddress, NonZeroU16),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PortRange(pub RangeInclusive<NonZeroU16>);

impl From<PortRange> for fnet_filter::PortRange {
    fn from(range: PortRange) -> Self {
        let PortRange(range) = range;
        Self { start: range.start().get(), end: range.end().get() }
    }
}

impl TryFrom<fnet_filter::PortRange> for PortRange {
    type Error = FidlConversionError;

    fn try_from(range: fnet_filter::PortRange) -> Result<Self, Self::Error> {
        let fnet_filter::PortRange { start, end } = range;
        if start > end {
            Err(FidlConversionError::InvalidPortRange)
        } else {
            let start = NonZeroU16::new(start).ok_or(FidlConversionError::UnspecifiedNatPort)?;
            let end = NonZeroU16::new(end).ok_or(FidlConversionError::UnspecifiedNatPort)?;
            Ok(Self(start..=end))
        }
    }
}

impl From<Action> for fnet_filter::Action {
    fn from(action: Action) -> Self {
        match action {
            Action::Accept => Self::Accept(fnet_filter::Empty {}),
            Action::Drop => Self::Drop(fnet_filter::Empty {}),
            Action::Jump(target) => Self::Jump(target),
            Action::Return => Self::Return_(fnet_filter::Empty {}),
            Action::TransparentProxy(proxy) => Self::TransparentProxy(match proxy {
                TransparentProxy::LocalAddr(addr) => {
                    fnet_filter::TransparentProxy_::LocalAddr(addr)
                }
                TransparentProxy::LocalPort(port) => {
                    fnet_filter::TransparentProxy_::LocalPort(port.get())
                }
                TransparentProxy::LocalAddrAndPort(addr, port) => {
                    fnet_filter::TransparentProxy_::LocalAddrAndPort(fnet_filter::SocketAddr {
                        addr,
                        port: port.get(),
                    })
                }
            }),
            Action::Redirect { dst_port } => Self::Redirect(fnet_filter::Redirect {
                dst_port: dst_port.map(Into::into),
                __source_breaking: SourceBreaking,
            }),
            Action::Masquerade { src_port } => Self::Masquerade(fnet_filter::Masquerade {
                src_port: src_port.map(Into::into),
                __source_breaking: SourceBreaking,
            }),
        }
    }
}

impl TryFrom<fnet_filter::Action> for Action {
    type Error = FidlConversionError;

    fn try_from(action: fnet_filter::Action) -> Result<Self, Self::Error> {
        match action {
            fnet_filter::Action::Accept(fnet_filter::Empty {}) => Ok(Self::Accept),
            fnet_filter::Action::Drop(fnet_filter::Empty {}) => Ok(Self::Drop),
            fnet_filter::Action::Jump(target) => Ok(Self::Jump(target)),
            fnet_filter::Action::Return_(fnet_filter::Empty {}) => Ok(Self::Return),
            fnet_filter::Action::TransparentProxy(proxy) => {
                Ok(Self::TransparentProxy(match proxy {
                    fnet_filter::TransparentProxy_::LocalAddr(addr) => {
                        TransparentProxy::LocalAddr(addr)
                    }
                    fnet_filter::TransparentProxy_::LocalPort(port) => {
                        let port = NonZeroU16::new(port)
                            .ok_or(FidlConversionError::UnspecifiedTransparentProxyPort)?;
                        TransparentProxy::LocalPort(port)
                    }
                    fnet_filter::TransparentProxy_::LocalAddrAndPort(fnet_filter::SocketAddr {
                        addr,
                        port,
                    }) => {
                        let port = NonZeroU16::new(port)
                            .ok_or(FidlConversionError::UnspecifiedTransparentProxyPort)?;
                        TransparentProxy::LocalAddrAndPort(addr, port)
                    }
                    fnet_filter::TransparentProxy_::__SourceBreaking { .. } => {
                        return Err(FidlConversionError::UnknownUnionVariant(
                            type_names::TRANSPARENT_PROXY,
                        ))
                    }
                }))
            }
            fnet_filter::Action::Redirect(fnet_filter::Redirect {
                dst_port,
                __source_breaking,
            }) => Ok(Self::Redirect { dst_port: dst_port.map(TryInto::try_into).transpose()? }),
            fnet_filter::Action::Masquerade(fnet_filter::Masquerade {
                src_port,
                __source_breaking,
            }) => Ok(Self::Masquerade { src_port: src_port.map(TryInto::try_into).transpose()? }),
            fnet_filter::Action::__SourceBreaking { .. } => {
                Err(FidlConversionError::UnknownUnionVariant(type_names::ACTION))
            }
        }
    }
}

/// Extension type for [`fnet_filter::Rule`].
#[derive(Debug, Clone, PartialEq)]
pub struct Rule {
    pub id: RuleId,
    pub matchers: Matchers,
    pub action: Action,
}

impl From<Rule> for fnet_filter::Rule {
    fn from(rule: Rule) -> Self {
        let Rule { id, matchers, action } = rule;
        Self { id: id.into(), matchers: matchers.into(), action: action.into() }
    }
}

impl TryFrom<fnet_filter::Rule> for Rule {
    type Error = FidlConversionError;

    fn try_from(rule: fnet_filter::Rule) -> Result<Self, Self::Error> {
        let fnet_filter::Rule { id, matchers, action } = rule;
        Ok(Self { id: id.into(), matchers: matchers.try_into()?, action: action.try_into()? })
    }
}

/// Extension type for [`fnet_filter::Resource`].
#[derive(Debug, Clone, PartialEq)]
pub enum Resource {
    Namespace(Namespace),
    Routine(Routine),
    Rule(Rule),
}

impl Resource {
    pub fn id(&self) -> ResourceId {
        match self {
            Self::Namespace(Namespace { id, domain: _ }) => ResourceId::Namespace(id.clone()),
            Self::Routine(Routine { id, routine_type: _ }) => ResourceId::Routine(id.clone()),
            Self::Rule(Rule { id, matchers: _, action: _ }) => ResourceId::Rule(id.clone()),
        }
    }
}

impl From<Resource> for fnet_filter::Resource {
    fn from(resource: Resource) -> Self {
        match resource {
            Resource::Namespace(namespace) => Self::Namespace(namespace.into()),
            Resource::Routine(routine) => Self::Routine(routine.into()),
            Resource::Rule(rule) => Self::Rule(rule.into()),
        }
    }
}

impl TryFrom<fnet_filter::Resource> for Resource {
    type Error = FidlConversionError;

    fn try_from(resource: fnet_filter::Resource) -> Result<Self, Self::Error> {
        match resource {
            fnet_filter::Resource::Namespace(namespace) => {
                Ok(Self::Namespace(namespace.try_into()?))
            }
            fnet_filter::Resource::Routine(routine) => Ok(Self::Routine(routine.try_into()?)),
            fnet_filter::Resource::Rule(rule) => Ok(Self::Rule(rule.try_into()?)),
            fnet_filter::Resource::__SourceBreaking { .. } => {
                Err(FidlConversionError::UnknownUnionVariant(type_names::RESOURCE))
            }
        }
    }
}

/// Extension type for [`fnet_filter::ControllerId`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ControllerId(pub String);

/// Extension type for [`fnet_filter::Event`].
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Existing(ControllerId, Resource),
    Idle,
    Added(ControllerId, Resource),
    Removed(ControllerId, ResourceId),
    EndOfUpdate,
}

impl From<Event> for fnet_filter::Event {
    fn from(event: Event) -> Self {
        match event {
            Event::Existing(controller, resource) => {
                let ControllerId(id) = controller;
                Self::Existing(fnet_filter::ExistingResource {
                    controller: id,
                    resource: resource.into(),
                })
            }
            Event::Idle => Self::Idle(fnet_filter::Empty {}),
            Event::Added(controller, resource) => {
                let ControllerId(id) = controller;
                Self::Added(fnet_filter::AddedResource {
                    controller: id,
                    resource: resource.into(),
                })
            }
            Event::Removed(controller, resource) => {
                let ControllerId(id) = controller;
                Self::Removed(fnet_filter::RemovedResource {
                    controller: id,
                    resource: resource.into(),
                })
            }
            Event::EndOfUpdate => Self::EndOfUpdate(fnet_filter::Empty {}),
        }
    }
}

impl TryFrom<fnet_filter::Event> for Event {
    type Error = FidlConversionError;

    fn try_from(event: fnet_filter::Event) -> Result<Self, Self::Error> {
        match event {
            fnet_filter::Event::Existing(fnet_filter::ExistingResource {
                controller,
                resource,
            }) => Ok(Self::Existing(ControllerId(controller), resource.try_into()?)),
            fnet_filter::Event::Idle(fnet_filter::Empty {}) => Ok(Self::Idle),
            fnet_filter::Event::Added(fnet_filter::AddedResource { controller, resource }) => {
                Ok(Self::Added(ControllerId(controller), resource.try_into()?))
            }
            fnet_filter::Event::Removed(fnet_filter::RemovedResource { controller, resource }) => {
                Ok(Self::Removed(ControllerId(controller), resource.try_into()?))
            }
            fnet_filter::Event::EndOfUpdate(fnet_filter::Empty {}) => Ok(Self::EndOfUpdate),
            fnet_filter::Event::__SourceBreaking { .. } => {
                Err(FidlConversionError::UnknownUnionVariant(type_names::EVENT))
            }
        }
    }
}

/// Filter watcher creation errors.
#[derive(Debug, Error)]
pub enum WatcherCreationError {
    #[error("failed to create filter watcher proxy: {0}")]
    CreateProxy(fidl::Error),
    #[error("failed to get filter watcher: {0}")]
    GetWatcher(fidl::Error),
}

/// Filter watcher `Watch` errors.
#[derive(Debug, Error)]
pub enum WatchError {
    /// The call to `Watch` returned a FIDL error.
    #[error("the call to `Watch()` failed: {0}")]
    Fidl(fidl::Error),
    /// The event returned by `Watch` encountered a conversion error.
    #[error("failed to convert event returned by `Watch()`: {0}")]
    Conversion(FidlConversionError),
    /// The server returned an empty batch of events.
    #[error("the call to `Watch()` returned an empty batch of events")]
    EmptyEventBatch,
}

/// Connects to the watcher protocol and converts the Hanging-Get style API into
/// an Event stream.
///
/// Each call to `Watch` returns a batch of events, which are flattened into a
/// single stream. If an error is encountered while calling `Watch` or while
/// converting the event, the stream is immediately terminated.
pub fn event_stream_from_state(
    state: fnet_filter::StateProxy,
) -> Result<impl Stream<Item = Result<Event, WatchError>>, WatcherCreationError> {
    let (watcher, server_end) = fidl::endpoints::create_proxy::<fnet_filter::WatcherMarker>()
        .map_err(WatcherCreationError::CreateProxy)?;
    state
        .get_watcher(&fnet_filter::WatcherOptions::default(), server_end)
        .map_err(WatcherCreationError::GetWatcher)?;

    let stream = futures::stream::try_unfold(watcher, |watcher| async {
        let events = watcher.watch().await.map_err(WatchError::Fidl)?;
        if events.is_empty() {
            return Err(WatchError::EmptyEventBatch);
        }

        let event_stream = futures::stream::iter(events).map(Ok).and_then(|event| {
            futures::future::ready(event.try_into().map_err(WatchError::Conversion))
        });
        Ok(Some((event_stream, watcher)))
    })
    .try_flatten();

    Ok(stream)
}

/// Errors returned by [`get_existing_resources`].
#[derive(Debug, Error)]
pub enum GetExistingResourcesError {
    /// There was an error in the event stream.
    #[error("there was an error in the event stream: {0}")]
    ErrorInStream(WatchError),
    /// There was an unexpected event in the event stream. Only `existing` or
    /// `idle` events are expected.
    #[error("there was an unexpected event in the event stream: {0:?}")]
    UnexpectedEvent(Event),
    /// A duplicate existing resource was reported in the event stream.
    #[error("a duplicate existing resource was reported")]
    DuplicateResource(Resource),
    /// The event stream unexpectedly ended.
    #[error("the event stream unexpectedly ended")]
    StreamEnded,
}

/// A trait for types holding filtering state that can be updated by change
/// events.
pub trait Update {
    /// Add the resource to the specified controller's state.
    ///
    /// Optionally returns a resource that has already been added to the
    /// controller with the same [`ResourceId`].
    fn add(&mut self, controller: ControllerId, resource: Resource) -> Option<Resource>;

    /// Remove the resource from the specified controller's state.
    ///
    /// Returns the removed resource, if present.
    fn remove(&mut self, controller: ControllerId, resource: &ResourceId) -> Option<Resource>;
}

impl Update for HashMap<ControllerId, HashMap<ResourceId, Resource>> {
    fn add(&mut self, controller: ControllerId, resource: Resource) -> Option<Resource> {
        self.entry(controller).or_default().insert(resource.id(), resource)
    }

    fn remove(&mut self, controller: ControllerId, resource: &ResourceId) -> Option<Resource> {
        self.get_mut(&controller)?.remove(resource)
    }
}

/// Collects all `existing` events from the stream, stopping once the `idle`
/// event is observed.
pub async fn get_existing_resources<C: Update + Default>(
    stream: impl Stream<Item = Result<Event, WatchError>>,
) -> Result<C, GetExistingResourcesError> {
    async_utils::fold::fold_while(
        stream,
        Ok(C::default()),
        |resources: Result<C, GetExistingResourcesError>, event| {
            let mut resources =
                resources.expect("`resources` must be `Ok`, because we stop folding on err");
            futures::future::ready(match event {
                Err(e) => FoldWhile::Done(Err(GetExistingResourcesError::ErrorInStream(e))),
                Ok(e) => match e {
                    Event::Existing(controller, resource) => {
                        if let Some(resource) = resources.add(controller, resource) {
                            FoldWhile::Done(Err(GetExistingResourcesError::DuplicateResource(
                                resource,
                            )))
                        } else {
                            FoldWhile::Continue(Ok(resources))
                        }
                    }
                    Event::Idle => FoldWhile::Done(Ok(resources)),
                    e @ (Event::Added(_, _) | Event::Removed(_, _) | Event::EndOfUpdate) => {
                        FoldWhile::Done(Err(GetExistingResourcesError::UnexpectedEvent(e)))
                    }
                },
            })
        },
    )
    .await
    .short_circuited()
    .map_err(|_resources| GetExistingResourcesError::StreamEnded)?
}

/// Errors returned by [`wait_for_condition`].
#[derive(Debug, Error)]
pub enum WaitForConditionError {
    /// There was an error in the event stream.
    #[error("there was an error in the event stream: {0}")]
    ErrorInStream(WatchError),
    /// There was an `Added` event for an already existing resource.
    #[error("observed an added event for an already existing resource: {0:?}")]
    AddedAlreadyExisting(Resource),
    /// There was a `Removed` event for a non-existent resource.
    #[error("observed a removed event for a non-existent resource: {0:?}")]
    RemovedNonExistent(ResourceId),
    /// The event stream unexpectedly ended.
    #[error("the event stream unexpectedly ended")]
    StreamEnded,
}

/// Wait for a condition on filtering state to be satisfied.
///
/// With the given `initial_state`, take events from `event_stream` and update
/// the state, calling `predicate` whenever the state changes. When predicates
/// returns `True` yield `Ok(())`.
pub async fn wait_for_condition<
    C: Update,
    S: Stream<Item = Result<Event, WatchError>>,
    F: Fn(&C) -> bool,
>(
    event_stream: S,
    initial_state: &mut C,
    predicate: F,
) -> Result<(), WaitForConditionError> {
    async_utils::fold::try_fold_while(
        event_stream.map_err(WaitForConditionError::ErrorInStream),
        initial_state,
        |resources: &mut C, event| {
            futures::future::ready(match event {
                Event::Existing(controller, resource) | Event::Added(controller, resource) => {
                    if let Some(resource) = resources.add(controller, resource) {
                        Err(WaitForConditionError::AddedAlreadyExisting(resource))
                    } else {
                        Ok(FoldWhile::Continue(resources))
                    }
                }
                Event::Removed(controller, resource) => resources
                    .remove(controller, &resource)
                    .map(|_| FoldWhile::Continue(resources))
                    .ok_or(WaitForConditionError::RemovedNonExistent(resource)),
                // Wait until a transactional update has been completed to call
                // the predicate so it's not run against partially-updated
                // state.
                Event::Idle | Event::EndOfUpdate => {
                    if predicate(&resources) {
                        Ok(FoldWhile::Done(()))
                    } else {
                        Ok(FoldWhile::Continue(resources))
                    }
                }
            })
        },
    )
    .await?
    .short_circuited()
    .map_err(|_resources: &mut C| WaitForConditionError::StreamEnded)
}

/// Namespace controller creation errors.
#[derive(Debug, Error)]
pub enum ControllerCreationError {
    #[error("failed to create namespace controller proxy: {0}")]
    CreateProxy(fidl::Error),
    #[error("failed to open namespace controller: {0}")]
    OpenController(fidl::Error),
    #[error("server did not emit OnIdAssigned event")]
    NoIdAssigned,
    #[error("failed to observe ID assignment event: {0}")]
    IdAssignment(fidl::Error),
}

/// Errors for individual changes pushed.
///
/// Extension type for the error variants of [`fnet_filter::ChangeValidationError`].
#[derive(Debug, Error, PartialEq)]
pub enum ChangeValidationError {
    #[error("change contains a resource that is missing a required field")]
    MissingRequiredField,
    #[error("rule specifies an invalid interface matcher")]
    InvalidInterfaceMatcher,
    #[error("rule specifies an invalid address matcher")]
    InvalidAddressMatcher,
    #[error("rule specifies an invalid port matcher")]
    InvalidPortMatcher,
    #[error("rule specifies an invalid transparent proxy action")]
    InvalidTransparentProxyAction,
    #[error("rule specifies an invalid NAT action")]
    InvalidNatAction,
    #[error("rule specifies an invalid port range")]
    InvalidPortRange,
}

impl TryFrom<fnet_filter::ChangeValidationError> for ChangeValidationError {
    type Error = FidlConversionError;

    fn try_from(error: fnet_filter::ChangeValidationError) -> Result<Self, Self::Error> {
        match error {
            fnet_filter::ChangeValidationError::MissingRequiredField => {
                Ok(Self::MissingRequiredField)
            }
            fnet_filter::ChangeValidationError::InvalidInterfaceMatcher => {
                Ok(Self::InvalidInterfaceMatcher)
            }
            fnet_filter::ChangeValidationError::InvalidAddressMatcher => {
                Ok(Self::InvalidAddressMatcher)
            }
            fnet_filter::ChangeValidationError::InvalidPortMatcher => Ok(Self::InvalidPortMatcher),
            fnet_filter::ChangeValidationError::InvalidTransparentProxyAction => {
                Ok(Self::InvalidTransparentProxyAction)
            }
            fnet_filter::ChangeValidationError::InvalidNatAction => Ok(Self::InvalidNatAction),
            fnet_filter::ChangeValidationError::InvalidPortRange => Ok(Self::InvalidPortRange),
            fnet_filter::ChangeValidationError::Ok
            | fnet_filter::ChangeValidationError::NotReached => {
                Err(FidlConversionError::NotAnError)
            }
            fnet_filter::ChangeValidationError::__SourceBreaking { unknown_ordinal: _ } => {
                Err(FidlConversionError::UnknownUnionVariant(type_names::CHANGE_VALIDATION_ERROR))
            }
        }
    }
}

/// Errors for the NamespaceController.PushChanges method.
#[derive(Debug, Error)]
pub enum PushChangesError {
    #[error("failed to call FIDL method: {0}")]
    CallMethod(fidl::Error),
    #[error("too many changes were pushed to the server")]
    TooManyChanges,
    #[error("invalid change(s) pushed: {0:?}")]
    ErrorOnChange(Vec<(Change, ChangeValidationError)>),
    #[error("unknown FIDL type: {0}")]
    FidlConversion(#[from] FidlConversionError),
}

/// Errors for individual changes committed.
///
/// Extension type for the error variants of [`fnet_filter::CommitError`].
#[derive(Debug, Error, PartialEq)]
pub enum ChangeCommitError {
    #[error("the change referred to an unknown namespace")]
    NamespaceNotFound,
    #[error("the change referred to an unknown routine")]
    RoutineNotFound,
    #[error("the change referred to an unknown rule")]
    RuleNotFound,
    #[error("the specified resource already exists")]
    AlreadyExists,
    #[error("the change includes a rule that jumps to an installed routine")]
    TargetRoutineIsInstalled,
}

impl TryFrom<fnet_filter::CommitError> for ChangeCommitError {
    type Error = FidlConversionError;

    fn try_from(error: fnet_filter::CommitError) -> Result<Self, Self::Error> {
        match error {
            fnet_filter::CommitError::NamespaceNotFound => Ok(Self::NamespaceNotFound),
            fnet_filter::CommitError::RoutineNotFound => Ok(Self::RoutineNotFound),
            fnet_filter::CommitError::RuleNotFound => Ok(Self::RuleNotFound),
            fnet_filter::CommitError::AlreadyExists => Ok(Self::AlreadyExists),
            fnet_filter::CommitError::TargetRoutineIsInstalled => {
                Ok(Self::TargetRoutineIsInstalled)
            }
            fnet_filter::CommitError::Ok | fnet_filter::CommitError::NotReached => {
                Err(FidlConversionError::NotAnError)
            }
            fnet_filter::CommitError::__SourceBreaking { unknown_ordinal: _ } => {
                Err(FidlConversionError::UnknownUnionVariant(type_names::COMMIT_ERROR))
            }
        }
    }
}

/// Errors for the NamespaceController.Commit method.
#[derive(Debug, Error)]
pub enum CommitError {
    #[error("failed to call FIDL method: {0}")]
    CallMethod(fidl::Error),
    #[error("rule has a matcher that is unavailable in its context: {0:?}")]
    RuleWithInvalidMatcher(RuleId),
    #[error("rule has an action that is invalid for its routine: {0:?}")]
    RuleWithInvalidAction(RuleId),
    #[error(
        "rule has a TransparentProxy action but not a valid transport protocol matcher: {0:?}"
    )]
    TransparentProxyWithInvalidMatcher(RuleId),
    #[error(
        "rule has a Redirect action that specifies a destination port but not a valid transport \
        protocol matcher: {0:?}"
    )]
    RedirectWithInvalidMatcher(RuleId),
    #[error(
        "rule has a Masquerade action that specifies a source port but not a valid transport \
        protocol matcher: {0:?}"
    )]
    MasqueradeWithInvalidMatcher(RuleId),
    #[error("routine forms a cycle {0:?}")]
    CyclicalRoutineGraph(RoutineId),
    #[error("invalid change was pushed: {0:?}")]
    ErrorOnChange(Vec<(Change, ChangeCommitError)>),
    #[error("unknown FIDL type: {0}")]
    FidlConversion(#[from] FidlConversionError),
}

/// Extension type for [`fnet_filter::Change`].
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Create(Resource),
    Remove(ResourceId),
}

impl From<Change> for fnet_filter::Change {
    fn from(change: Change) -> Self {
        match change {
            Change::Create(resource) => Self::Create(resource.into()),
            Change::Remove(resource) => Self::Remove(resource.into()),
        }
    }
}

impl TryFrom<fnet_filter::Change> for Change {
    type Error = FidlConversionError;

    fn try_from(change: fnet_filter::Change) -> Result<Self, Self::Error> {
        match change {
            fnet_filter::Change::Create(resource) => Ok(Self::Create(resource.try_into()?)),
            fnet_filter::Change::Remove(resource) => Ok(Self::Remove(resource.try_into()?)),
            fnet_filter::Change::__SourceBreaking { .. } => {
                Err(FidlConversionError::UnknownUnionVariant(type_names::CHANGE))
            }
        }
    }
}

/// A controller for filtering state.
pub struct Controller {
    controller: fnet_filter::NamespaceControllerProxy,
    // The client provides an ID when creating a new controller, but the server
    // may need to assign a different ID to avoid conflicts; either way, the
    // server informs the client of the final `ControllerId` on creation.
    id: ControllerId,
    // Changes that have been pushed to the server but not yet committed. This
    // allows the `Controller` to report more informative errors by correlating
    // error codes with particular changes.
    pending_changes: Vec<Change>,
}

impl Controller {
    pub async fn new_root(
        root: &fnet_root::FilterProxy,
        ControllerId(id): &ControllerId,
    ) -> Result<Self, ControllerCreationError> {
        let (controller, server_end) =
            fidl::endpoints::create_proxy().map_err(ControllerCreationError::CreateProxy)?;
        root.open_controller(id, server_end).map_err(ControllerCreationError::OpenController)?;

        let fnet_filter::NamespaceControllerEvent::OnIdAssigned { id } = controller
            .take_event_stream()
            .next()
            .await
            .ok_or(ControllerCreationError::NoIdAssigned)?
            .map_err(ControllerCreationError::IdAssignment)?;
        Ok(Self { controller, id: ControllerId(id), pending_changes: Vec::new() })
    }

    /// Creates a new `Controller`.
    ///
    /// Note that the provided `ControllerId` may need to be modified server-
    /// side to avoid collisions; to obtain the final ID assigned to the
    /// `Controller`, use the `id` method.
    pub async fn new(
        control: &fnet_filter::ControlProxy,
        ControllerId(id): &ControllerId,
    ) -> Result<Self, ControllerCreationError> {
        let (controller, server_end) =
            fidl::endpoints::create_proxy().map_err(ControllerCreationError::CreateProxy)?;
        control.open_controller(id, server_end).map_err(ControllerCreationError::OpenController)?;

        let fnet_filter::NamespaceControllerEvent::OnIdAssigned { id } = controller
            .take_event_stream()
            .next()
            .await
            .ok_or(ControllerCreationError::NoIdAssigned)?
            .map_err(ControllerCreationError::IdAssignment)?;
        Ok(Self { controller, id: ControllerId(id), pending_changes: Vec::new() })
    }

    pub fn id(&self) -> &ControllerId {
        &self.id
    }

    pub async fn push_changes(&mut self, changes: Vec<Change>) -> Result<(), PushChangesError> {
        let fidl_changes = changes.iter().cloned().map(Into::into).collect::<Vec<_>>();
        let result = self
            .controller
            .push_changes(&fidl_changes)
            .await
            .map_err(PushChangesError::CallMethod)?;
        handle_change_validation_result(result, &changes)?;
        // Maintain a client-side copy of the pending changes we've pushed to
        // the server in order to provide better error messages if a commit
        // fails.
        self.pending_changes.extend(changes);
        Ok(())
    }

    async fn commit_with_options(
        &mut self,
        options: fnet_filter::CommitOptions,
    ) -> Result<(), CommitError> {
        let committed_changes = std::mem::take(&mut self.pending_changes);
        let result = self.controller.commit(options).await.map_err(CommitError::CallMethod)?;
        handle_commit_result(result, committed_changes)
    }

    pub async fn commit(&mut self) -> Result<(), CommitError> {
        self.commit_with_options(fnet_filter::CommitOptions::default()).await
    }

    pub async fn commit_idempotent(&mut self) -> Result<(), CommitError> {
        self.commit_with_options(fnet_filter::CommitOptions {
            idempotent: Some(true),
            __source_breaking: SourceBreaking,
        })
        .await
    }
}

pub(crate) fn handle_change_validation_result(
    change_validation_result: fnet_filter::ChangeValidationResult,
    changes: &Vec<Change>,
) -> Result<(), PushChangesError> {
    match change_validation_result {
        fnet_filter::ChangeValidationResult::Ok(fnet_filter::Empty {}) => Ok(()),
        fnet_filter::ChangeValidationResult::TooManyChanges(fnet_filter::Empty {}) => {
            Err(PushChangesError::TooManyChanges)
        }
        fnet_filter::ChangeValidationResult::ErrorOnChange(results) => {
            let errors: Result<_, PushChangesError> =
                changes.iter().zip(results).try_fold(Vec::new(), |mut errors, (change, result)| {
                    match result {
                        fnet_filter::ChangeValidationError::Ok
                        | fnet_filter::ChangeValidationError::NotReached => Ok(errors),
                        error @ (fnet_filter::ChangeValidationError::MissingRequiredField
                        | fnet_filter::ChangeValidationError::InvalidInterfaceMatcher
                        | fnet_filter::ChangeValidationError::InvalidAddressMatcher
                        | fnet_filter::ChangeValidationError::InvalidPortMatcher
                        | fnet_filter::ChangeValidationError::InvalidTransparentProxyAction
                        | fnet_filter::ChangeValidationError::InvalidNatAction
                        | fnet_filter::ChangeValidationError::InvalidPortRange) => {
                            let error = error
                                .try_into()
                                .expect("`Ok` and `NotReached` are handled in another arm");
                            errors.push((change.clone(), error));
                            Ok(errors)
                        }
                        fnet_filter::ChangeValidationError::__SourceBreaking { .. } => {
                            Err(FidlConversionError::UnknownUnionVariant(
                                type_names::CHANGE_VALIDATION_ERROR,
                            )
                            .into())
                        }
                    }
                });
            Err(PushChangesError::ErrorOnChange(errors?))
        }
        fnet_filter::ChangeValidationResult::__SourceBreaking { .. } => {
            Err(FidlConversionError::UnknownUnionVariant(type_names::CHANGE_VALIDATION_RESULT)
                .into())
        }
    }
}

pub(crate) fn handle_commit_result(
    commit_result: fnet_filter::CommitResult,
    committed_changes: Vec<Change>,
) -> Result<(), CommitError> {
    match commit_result {
        fnet_filter::CommitResult::Ok(fnet_filter::Empty {}) => Ok(()),
        fnet_filter::CommitResult::RuleWithInvalidMatcher(rule_id) => {
            Err(CommitError::RuleWithInvalidMatcher(rule_id.into()))
        }
        fnet_filter::CommitResult::RuleWithInvalidAction(rule_id) => {
            Err(CommitError::RuleWithInvalidAction(rule_id.into()))
        }
        fnet_filter::CommitResult::TransparentProxyWithInvalidMatcher(rule_id) => {
            Err(CommitError::TransparentProxyWithInvalidMatcher(rule_id.into()))
        }
        fnet_filter::CommitResult::RedirectWithInvalidMatcher(rule_id) => {
            Err(CommitError::RedirectWithInvalidMatcher(rule_id.into()))
        }
        fnet_filter::CommitResult::MasqueradeWithInvalidMatcher(rule_id) => {
            Err(CommitError::MasqueradeWithInvalidMatcher(rule_id.into()))
        }
        fnet_filter::CommitResult::CyclicalRoutineGraph(routine_id) => {
            Err(CommitError::CyclicalRoutineGraph(routine_id.into()))
        }
        fnet_filter::CommitResult::ErrorOnChange(results) => {
            let errors: Result<_, CommitError> = committed_changes
                .into_iter()
                .zip(results)
                .try_fold(Vec::new(), |mut errors, (change, result)| match result {
                    fnet_filter::CommitError::Ok | fnet_filter::CommitError::NotReached => {
                        Ok(errors)
                    }
                    error @ (fnet_filter::CommitError::NamespaceNotFound
                    | fnet_filter::CommitError::RoutineNotFound
                    | fnet_filter::CommitError::RuleNotFound
                    | fnet_filter::CommitError::AlreadyExists
                    | fnet_filter::CommitError::TargetRoutineIsInstalled) => {
                        let error = error
                            .try_into()
                            .expect("`Ok` and `NotReached` are handled in another arm");
                        errors.push((change, error));
                        Ok(errors)
                    }
                    fnet_filter::CommitError::__SourceBreaking { .. } => {
                        Err(FidlConversionError::UnknownUnionVariant(type_names::COMMIT_ERROR)
                            .into())
                    }
                });
            Err(CommitError::ErrorOnChange(errors?))
        }
        fnet_filter::CommitResult::__SourceBreaking { .. } => {
            Err(FidlConversionError::UnknownUnionVariant(type_names::COMMIT_RESULT).into())
        }
    }
}

#[cfg(test)]
mod tests {

    use assert_matches::assert_matches;
    use const_unwrap::const_unwrap_option;
    use futures::channel::mpsc;
    use futures::task::Poll;
    use futures::{FutureExt as _, SinkExt as _};
    use net_declare::{fidl_ip, fidl_subnet};
    use test_case::test_case;

    use super::*;

    #[test_case(
        fnet_filter::ResourceId::Namespace(String::from("namespace")),
        ResourceId::Namespace(NamespaceId(String::from("namespace")));
        "NamespaceId"
    )]
    #[test_case(fnet_filter::Domain::Ipv4, Domain::Ipv4; "Domain")]
    #[test_case(
        fnet_filter::Namespace {
            id: Some(String::from("namespace")),
            domain: Some(fnet_filter::Domain::Ipv4),
            ..Default::default()
        },
        Namespace { id: NamespaceId(String::from("namespace")), domain: Domain::Ipv4 };
        "Namespace"
    )]
    #[test_case(fnet_filter::IpInstallationHook::Egress, IpHook::Egress; "IpHook")]
    #[test_case(fnet_filter::NatInstallationHook::Egress, NatHook::Egress; "NatHook")]
    #[test_case(
        fnet_filter::InstalledIpRoutine {
            hook: Some(fnet_filter::IpInstallationHook::Egress),
            priority: Some(1),
            ..Default::default()
        },
        InstalledIpRoutine {
            hook: IpHook::Egress,
            priority: 1,
        };
        "InstalledIpRoutine"
    )]
    #[test_case(
        fnet_filter::RoutineType::Ip(fnet_filter::IpRoutine {
            installation: Some(fnet_filter::InstalledIpRoutine {
                hook: Some(fnet_filter::IpInstallationHook::LocalEgress),
                priority: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        }),
        RoutineType::Ip(Some(InstalledIpRoutine { hook: IpHook::LocalEgress, priority: 1 }));
        "RoutineType"
    )]
    #[test_case(
        fnet_filter::Routine {
            id: Some(fnet_filter::RoutineId {
                namespace: String::from("namespace"),
                name: String::from("routine"),
            }),
            type_: Some(fnet_filter::RoutineType::Nat(fnet_filter::NatRoutine::default())),
            ..Default::default()
        },
        Routine {
            id: RoutineId {
                namespace: NamespaceId(String::from("namespace")),
                name: String::from("routine"),
            },
            routine_type: RoutineType::Nat(None),
        };
        "Routine"
    )]
    #[test_case(
        fnet_filter::InterfaceMatcher::Id(1),
        InterfaceMatcher::Id(const_unwrap_option(NonZeroU64::new(1)));
        "InterfaceMatcher"
    )]
    #[test_case(
        fnet_filter::AddressMatcherType::Subnet(fidl_subnet!("192.0.2.0/24")),
        AddressMatcherType::Subnet(Subnet(fidl_subnet!("192.0.2.0/24")));
        "AddressMatcherType"
    )]
    #[test_case(
        fnet_filter::AddressMatcher {
            matcher: fnet_filter::AddressMatcherType::Subnet(fidl_subnet!("192.0.2.0/24")),
            invert: true,
        },
        AddressMatcher {
            matcher: AddressMatcherType::Subnet(Subnet(fidl_subnet!("192.0.2.0/24"))),
            invert: true,
        };
        "AddressMatcher"
    )]
    #[test_case(
        fnet_filter::AddressRange {
            start: fidl_ip!("192.0.2.0"),
            end: fidl_ip!("192.0.2.1"),
        },
        AddressRange {
            range: fidl_ip!("192.0.2.0")..=fidl_ip!("192.0.2.1"),
        };
        "AddressRange"
    )]
    #[test_case(
        fnet_filter::TransportProtocol::Udp(fnet_filter::UdpMatcher {
            src_port: Some(fnet_filter::PortMatcher { start: 1024, end: u16::MAX, invert: false }),
            dst_port: None,
            ..Default::default()
        }),
        TransportProtocolMatcher::Udp {
            src_port: Some(PortMatcher { range: 1024..=u16::MAX, invert: false }),
            dst_port: None,
        };
        "TransportProtocol"
    )]
    #[test_case(
        fnet_filter::Matchers {
            in_interface: Some(fnet_filter::InterfaceMatcher::Name(String::from("wlan"))),
            transport_protocol: Some(fnet_filter::TransportProtocol::Tcp(fnet_filter::TcpMatcher {
                src_port: None,
                dst_port: Some(fnet_filter::PortMatcher { start: 22, end: 22, invert: false }),
                ..Default::default()
            })),
            ..Default::default()
        },
        Matchers {
            in_interface: Some(InterfaceMatcher::Name(String::from("wlan"))),
            transport_protocol: Some(TransportProtocolMatcher::Tcp {
                src_port: None,
                dst_port: Some(PortMatcher { range: 22..=22, invert: false }),
            }),
            ..Default::default()
        };
        "Matchers"
    )]
    #[test_case(
        fnet_filter::Action::Accept(fnet_filter::Empty {}),
        Action::Accept;
        "Action"
    )]
    #[test_case(
        fnet_filter::Rule {
            id: fnet_filter::RuleId {
                routine: fnet_filter::RoutineId {
                    namespace: String::from("namespace"),
                    name: String::from("routine"),
                },
                index: 1,
            },
            matchers: fnet_filter::Matchers {
                transport_protocol: Some(fnet_filter::TransportProtocol::Icmp(
                    fnet_filter::IcmpMatcher::default()
                )),
                ..Default::default()
            },
            action: fnet_filter::Action::Drop(fnet_filter::Empty {}),
        },
        Rule {
            id: RuleId {
                routine: RoutineId {
                    namespace: NamespaceId(String::from("namespace")),
                    name: String::from("routine"),
                },
                index: 1,
            },
            matchers: Matchers {
                transport_protocol: Some(TransportProtocolMatcher::Icmp),
                ..Default::default()
            },
            action: Action::Drop,
        };
        "Rule"
    )]
    #[test_case(
        fnet_filter::Resource::Namespace(fnet_filter::Namespace {
            id: Some(String::from("namespace")),
            domain: Some(fnet_filter::Domain::Ipv4),
            ..Default::default()
        }),
        Resource::Namespace(Namespace {
            id: NamespaceId(String::from("namespace")),
            domain: Domain::Ipv4
        });
        "Resource"
    )]
    #[test_case(
        fnet_filter::Event::EndOfUpdate(fnet_filter::Empty {}),
        Event::EndOfUpdate;
        "Event"
    )]
    #[test_case(
        fnet_filter::Change::Remove(fnet_filter::ResourceId::Namespace(String::from("namespace"))),
        Change::Remove(ResourceId::Namespace(NamespaceId(String::from("namespace"))));
        "Change"
    )]
    fn convert_from_fidl_and_back<F, E>(fidl_type: F, local_type: E)
    where
        E: TryFrom<F> + Clone + Debug + PartialEq,
        <E as TryFrom<F>>::Error: Debug + PartialEq,
        F: From<E> + Clone + Debug + PartialEq,
    {
        assert_eq!(fidl_type.clone().try_into(), Ok(local_type.clone()));
        assert_eq!(<_ as Into<F>>::into(local_type), fidl_type.clone());
    }

    #[test]
    fn resource_id_try_from_unknown_variant() {
        assert_eq!(
            ResourceId::try_from(fnet_filter::ResourceId::__SourceBreaking { unknown_ordinal: 0 }),
            Err(FidlConversionError::UnknownUnionVariant(type_names::RESOURCE_ID))
        );
    }

    #[test]
    fn domain_try_from_unknown_variant() {
        assert_eq!(
            Domain::try_from(fnet_filter::Domain::__SourceBreaking { unknown_ordinal: 0 }),
            Err(FidlConversionError::UnknownUnionVariant(type_names::DOMAIN))
        );
    }

    #[test]
    fn namespace_try_from_missing_properties() {
        assert_eq!(
            Namespace::try_from(fnet_filter::Namespace {
                id: None,
                domain: Some(fnet_filter::Domain::Ipv4),
                ..Default::default()
            }),
            Err(FidlConversionError::MissingNamespaceId)
        );
        assert_eq!(
            Namespace::try_from(fnet_filter::Namespace {
                id: Some(String::from("namespace")),
                domain: None,
                ..Default::default()
            }),
            Err(FidlConversionError::MissingNamespaceDomain)
        );
    }

    #[test]
    fn ip_installation_hook_try_from_unknown_variant() {
        assert_eq!(
            IpHook::try_from(fnet_filter::IpInstallationHook::__SourceBreaking {
                unknown_ordinal: 0
            }),
            Err(FidlConversionError::UnknownUnionVariant(type_names::IP_INSTALLATION_HOOK))
        );
    }

    #[test]
    fn nat_installation_hook_try_from_unknown_variant() {
        assert_eq!(
            NatHook::try_from(fnet_filter::NatInstallationHook::__SourceBreaking {
                unknown_ordinal: 0
            }),
            Err(FidlConversionError::UnknownUnionVariant(type_names::NAT_INSTALLATION_HOOK))
        );
    }

    #[test]
    fn installed_ip_routine_try_from_missing_hook() {
        assert_eq!(
            InstalledIpRoutine::try_from(fnet_filter::InstalledIpRoutine {
                hook: None,
                ..Default::default()
            }),
            Err(FidlConversionError::MissingIpInstallationHook)
        );
    }

    #[test]
    fn installed_nat_routine_try_from_missing_hook() {
        assert_eq!(
            InstalledNatRoutine::try_from(fnet_filter::InstalledNatRoutine {
                hook: None,
                ..Default::default()
            }),
            Err(FidlConversionError::MissingNatInstallationHook)
        );
    }

    #[test]
    fn routine_type_try_from_unknown_variant() {
        assert_eq!(
            RoutineType::try_from(fnet_filter::RoutineType::__SourceBreaking {
                unknown_ordinal: 0
            }),
            Err(FidlConversionError::UnknownUnionVariant(type_names::ROUTINE_TYPE))
        );
    }

    #[test]
    fn routine_try_from_missing_properties() {
        assert_eq!(
            Routine::try_from(fnet_filter::Routine { id: None, ..Default::default() }),
            Err(FidlConversionError::MissingRoutineId)
        );
        assert_eq!(
            Routine::try_from(fnet_filter::Routine {
                id: Some(fnet_filter::RoutineId {
                    namespace: String::from("namespace"),
                    name: String::from("routine"),
                }),
                type_: None,
                ..Default::default()
            }),
            Err(FidlConversionError::MissingRoutineType)
        );
    }

    #[test]
    fn interface_matcher_try_from_unknown_variant() {
        assert_eq!(
            InterfaceMatcher::try_from(fnet_filter::InterfaceMatcher::__SourceBreaking {
                unknown_ordinal: 0
            }),
            Err(FidlConversionError::UnknownUnionVariant(type_names::INTERFACE_MATCHER))
        );
    }

    #[test]
    fn interface_matcher_try_from_invalid() {
        assert_eq!(
            InterfaceMatcher::try_from(fnet_filter::InterfaceMatcher::Id(0)),
            Err(FidlConversionError::ZeroInterfaceId)
        );
    }

    #[test]
    fn address_matcher_type_try_from_unknown_variant() {
        assert_eq!(
            AddressMatcherType::try_from(fnet_filter::AddressMatcherType::__SourceBreaking {
                unknown_ordinal: 0
            }),
            Err(FidlConversionError::UnknownUnionVariant(type_names::ADDRESS_MATCHER_TYPE))
        );
    }

    #[test]
    fn subnet_try_from_invalid() {
        assert_eq!(
            Subnet::try_from(fnet::Subnet { addr: fidl_ip!("192.0.2.1"), prefix_len: 33 }),
            Err(FidlConversionError::SubnetPrefixTooLong)
        );
        assert_eq!(
            Subnet::try_from(fidl_subnet!("192.0.2.1/24")),
            Err(FidlConversionError::SubnetHostBitsSet)
        );
    }

    #[test]
    fn address_range_try_from_invalid() {
        assert_eq!(
            AddressRange::try_from(fnet_filter::AddressRange {
                start: fidl_ip!("192.0.2.1"),
                end: fidl_ip!("192.0.2.0"),
            }),
            Err(FidlConversionError::InvalidAddressRange)
        );
        assert_eq!(
            AddressRange::try_from(fnet_filter::AddressRange {
                start: fidl_ip!("2001:db8::1"),
                end: fidl_ip!("2001:db8::"),
            }),
            Err(FidlConversionError::InvalidAddressRange)
        );
    }

    #[test]
    fn address_range_try_from_family_mismatch() {
        assert_eq!(
            AddressRange::try_from(fnet_filter::AddressRange {
                start: fidl_ip!("192.0.2.0"),
                end: fidl_ip!("2001:db8::"),
            }),
            Err(FidlConversionError::AddressRangeFamilyMismatch)
        );
    }

    #[test]
    fn port_matcher_try_from_invalid() {
        assert_eq!(
            PortMatcher::try_from(fnet_filter::PortMatcher { start: 1, end: 0, invert: false }),
            Err(FidlConversionError::InvalidPortMatcherRange)
        );
    }

    #[test]
    fn transport_protocol_try_from_unknown_variant() {
        assert_eq!(
            TransportProtocolMatcher::try_from(fnet_filter::TransportProtocol::__SourceBreaking {
                unknown_ordinal: 0
            }),
            Err(FidlConversionError::UnknownUnionVariant(type_names::TRANSPORT_PROTOCOL))
        );
    }

    #[test]
    fn action_try_from_unknown_variant() {
        assert_eq!(
            Action::try_from(fnet_filter::Action::__SourceBreaking { unknown_ordinal: 0 }),
            Err(FidlConversionError::UnknownUnionVariant(type_names::ACTION))
        );
    }

    #[test]
    fn resource_try_from_unknown_variant() {
        assert_eq!(
            Resource::try_from(fnet_filter::Resource::__SourceBreaking { unknown_ordinal: 0 }),
            Err(FidlConversionError::UnknownUnionVariant(type_names::RESOURCE))
        );
    }

    #[test]
    fn event_try_from_unknown_variant() {
        assert_eq!(
            Event::try_from(fnet_filter::Event::__SourceBreaking { unknown_ordinal: 0 }),
            Err(FidlConversionError::UnknownUnionVariant(type_names::EVENT))
        );
    }

    #[test]
    fn change_try_from_unknown_variant() {
        assert_eq!(
            Change::try_from(fnet_filter::Change::__SourceBreaking { unknown_ordinal: 0 }),
            Err(FidlConversionError::UnknownUnionVariant(type_names::CHANGE))
        );
    }

    fn test_controller_a() -> ControllerId {
        ControllerId(String::from("test-controller-a"))
    }

    fn test_controller_b() -> ControllerId {
        ControllerId(String::from("test-controller-b"))
    }

    pub(crate) fn test_resource_id() -> ResourceId {
        ResourceId::Namespace(NamespaceId(String::from("test-namespace")))
    }

    pub(crate) fn test_resource() -> Resource {
        Resource::Namespace(Namespace {
            id: NamespaceId(String::from("test-namespace")),
            domain: Domain::AllIp,
        })
    }

    pub(crate) fn invalid_resource() -> Resource {
        Resource::Rule(Rule {
            id: RuleId {
                routine: RoutineId {
                    namespace: NamespaceId(String::from("namespace")),
                    name: String::from("routine"),
                },
                index: 0,
            },
            matchers: Matchers {
                transport_protocol: Some(TransportProtocolMatcher::Tcp {
                    #[allow(clippy::reversed_empty_ranges)]
                    src_port: Some(PortMatcher { range: u16::MAX..=0, invert: false }),
                    dst_port: None,
                }),
                ..Default::default()
            },
            action: Action::Drop,
        })
    }

    pub(crate) fn unknown_resource_id() -> ResourceId {
        ResourceId::Namespace(NamespaceId(String::from("does-not-exist")))
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn event_stream_from_state_conversion_error() {
        let (proxy, mut request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnet_filter::StateMarker>().unwrap();
        let stream = event_stream_from_state(proxy).expect("get event stream");
        futures::pin_mut!(stream);

        let send_invalid_event = async {
            let fnet_filter::StateRequest::GetWatcher { options: _, request, control_handle: _ } =
                request_stream
                    .next()
                    .await
                    .expect("client should call state")
                    .expect("request should not error");
            let fnet_filter::WatcherRequest::Watch { responder } = request
                .into_stream()
                .next()
                .await
                .expect("client should call watch")
                .expect("request should not error");
            responder
                .send(&[fnet_filter::Event::Added(fnet_filter::AddedResource {
                    controller: String::from("controller"),
                    resource: fnet_filter::Resource::Namespace(fnet_filter::Namespace {
                        id: None,
                        domain: None,
                        ..Default::default()
                    }),
                })])
                .expect("send batch with invalid event");
        };
        let ((), result) = futures::future::join(send_invalid_event, stream.next()).await;
        assert_matches!(
            result,
            Some(Err(WatchError::Conversion(FidlConversionError::MissingNamespaceId)))
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn event_stream_from_state_empty_event_batch() {
        let (proxy, mut request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnet_filter::StateMarker>().unwrap();
        let stream = event_stream_from_state(proxy).expect("get event stream");
        futures::pin_mut!(stream);

        let send_empty_batch = async {
            let fnet_filter::StateRequest::GetWatcher { options: _, request, control_handle: _ } =
                request_stream
                    .next()
                    .await
                    .expect("client should call state")
                    .expect("request should not error");
            let fnet_filter::WatcherRequest::Watch { responder } = request
                .into_stream()
                .next()
                .await
                .expect("client should call watch")
                .expect("request should not error");
            responder.send(&[]).expect("send empty batch");
        };
        let ((), result) = futures::future::join(send_empty_batch, stream.next()).await;
        assert_matches!(result, Some(Err(WatchError::EmptyEventBatch)));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn get_existing_resources_success() {
        let event_stream = futures::stream::iter([
            Ok(Event::Existing(test_controller_a(), test_resource())),
            Ok(Event::Existing(test_controller_b(), test_resource())),
            Ok(Event::Idle),
            Ok(Event::Removed(test_controller_a(), test_resource_id())),
        ]);
        futures::pin_mut!(event_stream);

        let existing = get_existing_resources::<HashMap<_, _>>(event_stream.by_ref())
            .await
            .expect("get existing resources");
        assert_eq!(
            existing,
            HashMap::from([
                (test_controller_a(), HashMap::from([(test_resource_id(), test_resource())])),
                (test_controller_b(), HashMap::from([(test_resource_id(), test_resource())])),
            ])
        );

        let trailing_events = event_stream.collect::<Vec<_>>().await;
        assert_matches!(
            &trailing_events[..],
            [Ok(Event::Removed(controller, resource))] if controller == &test_controller_a() &&
                                                           resource == &test_resource_id()
        );
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn get_existing_resources_error_in_stream() {
        let event_stream =
            futures::stream::once(futures::future::ready(Err(WatchError::EmptyEventBatch)));
        futures::pin_mut!(event_stream);
        assert_matches!(
            get_existing_resources::<HashMap<_, _>>(event_stream).await,
            Err(GetExistingResourcesError::ErrorInStream(WatchError::EmptyEventBatch))
        )
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn get_existing_resources_unexpected_event() {
        let event_stream = futures::stream::once(futures::future::ready(Ok(Event::EndOfUpdate)));
        futures::pin_mut!(event_stream);
        assert_matches!(
            get_existing_resources::<HashMap<_, _>>(event_stream).await,
            Err(GetExistingResourcesError::UnexpectedEvent(Event::EndOfUpdate))
        )
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn get_existing_resources_duplicate_resource() {
        let event_stream = futures::stream::iter([
            Ok(Event::Existing(test_controller_a(), test_resource())),
            Ok(Event::Existing(test_controller_a(), test_resource())),
        ]);
        futures::pin_mut!(event_stream);
        assert_matches!(
            get_existing_resources::<HashMap<_, _>>(event_stream).await,
            Err(GetExistingResourcesError::DuplicateResource(resource))
                if resource == test_resource()
        )
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn get_existing_resources_stream_ended() {
        let event_stream = futures::stream::once(futures::future::ready(Ok(Event::Existing(
            test_controller_a(),
            test_resource(),
        ))));
        futures::pin_mut!(event_stream);
        assert_matches!(
            get_existing_resources::<HashMap<_, _>>(event_stream).await,
            Err(GetExistingResourcesError::StreamEnded)
        )
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn wait_for_condition_add_remove() {
        let mut state = HashMap::new();

        // Verify that checking for the presence of a resource blocks until the
        // resource is added.
        let has_resource = |resources: &HashMap<_, HashMap<_, _>>| {
            resources.get(&test_controller_a()).map_or(false, |controller| {
                controller
                    .get(&test_resource_id())
                    .map_or(false, |resource| resource == &test_resource())
            })
        };
        assert_matches!(
            wait_for_condition(futures::stream::pending(), &mut state, has_resource).now_or_never(),
            None
        );
        assert!(state.is_empty());
        assert_matches!(
            wait_for_condition(
                futures::stream::iter([
                    Ok(Event::Added(test_controller_b(), test_resource())),
                    Ok(Event::EndOfUpdate),
                    Ok(Event::Added(test_controller_a(), test_resource())),
                    Ok(Event::EndOfUpdate),
                ]),
                &mut state,
                has_resource
            )
            .now_or_never(),
            Some(Ok(()))
        );
        assert_eq!(
            state,
            HashMap::from([
                (test_controller_a(), HashMap::from([(test_resource_id(), test_resource())])),
                (test_controller_b(), HashMap::from([(test_resource_id(), test_resource())])),
            ])
        );

        // Re-add the resource and observe an error.
        assert_matches!(
            wait_for_condition(
                futures::stream::iter([
                    Ok(Event::Added(test_controller_a(), test_resource())),
                    Ok(Event::EndOfUpdate),
                ]),
                &mut state,
                has_resource
            )
            .now_or_never(),
            Some(Err(WaitForConditionError::AddedAlreadyExisting(r))) if r == test_resource()
        );
        assert_eq!(
            state,
            HashMap::from([
                (test_controller_a(), HashMap::from([(test_resource_id(), test_resource())])),
                (test_controller_b(), HashMap::from([(test_resource_id(), test_resource())])),
            ])
        );

        // Verify that checking for the absence of a resource blocks until the
        // resource is removed.
        let does_not_have_resource = |resources: &HashMap<_, HashMap<_, _>>| {
            resources.get(&test_controller_a()).map_or(false, |controller| controller.is_empty())
        };
        assert_matches!(
            wait_for_condition(futures::stream::pending(), &mut state, does_not_have_resource)
                .now_or_never(),
            None
        );
        assert_eq!(
            state,
            HashMap::from([
                (test_controller_a(), HashMap::from([(test_resource_id(), test_resource())])),
                (test_controller_b(), HashMap::from([(test_resource_id(), test_resource())])),
            ])
        );
        assert_matches!(
            wait_for_condition(
                futures::stream::iter([
                    Ok(Event::Removed(test_controller_b(), test_resource_id())),
                    Ok(Event::EndOfUpdate),
                    Ok(Event::Removed(test_controller_a(), test_resource_id())),
                    Ok(Event::EndOfUpdate),
                ]),
                &mut state,
                does_not_have_resource
            )
            .now_or_never(),
            Some(Ok(()))
        );
        assert_eq!(
            state,
            HashMap::from([
                (test_controller_a(), HashMap::new()),
                (test_controller_b(), HashMap::new()),
            ])
        );

        // Remove a non-existent resource and observe an error.
        assert_matches!(
            wait_for_condition(
                futures::stream::iter([
                    Ok(Event::Removed(test_controller_a(), test_resource_id())),
                    Ok(Event::EndOfUpdate),
                ]),
                &mut state,
                does_not_have_resource
            ).now_or_never(),
            Some(Err(WaitForConditionError::RemovedNonExistent(r))) if r == test_resource_id()
        );
        assert_eq!(
            state,
            HashMap::from([
                (test_controller_a(), HashMap::new()),
                (test_controller_b(), HashMap::new()),
            ])
        );
    }

    #[test]
    fn predicate_not_tested_until_update_complete() {
        let mut state = HashMap::new();
        let (mut tx, rx) = mpsc::unbounded();

        let wait = wait_for_condition(rx, &mut state, |state| !state.is_empty()).fuse();
        futures::pin_mut!(wait);

        // Sending an `Added` event should *not* allow the wait operation to
        // complete, because the predicate should only be tested once the full
        // update has been observed.
        let mut exec = fuchsia_async::TestExecutor::new();
        exec.run_singlethreaded(async {
            tx.send(Ok(Event::Added(test_controller_a(), test_resource())))
                .await
                .expect("receiver should not be closed");
        });
        assert_matches!(exec.run_until_stalled(&mut wait), Poll::Pending);

        exec.run_singlethreaded(async {
            tx.send(Ok(Event::EndOfUpdate)).await.expect("receiver should not be closed");
            wait.await.expect("condition should be satisfied once update is complete");
        });
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn wait_for_condition_error_in_stream() {
        let mut state = HashMap::new();
        let event_stream =
            futures::stream::once(futures::future::ready(Err(WatchError::EmptyEventBatch)));
        assert_matches!(
            wait_for_condition(event_stream, &mut state, |_| true).await,
            Err(WaitForConditionError::ErrorInStream(WatchError::EmptyEventBatch))
        );
        assert!(state.is_empty());
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn wait_for_condition_stream_ended() {
        let mut state = HashMap::new();
        let event_stream = futures::stream::empty();
        assert_matches!(
            wait_for_condition(event_stream, &mut state, |_| true).await,
            Err(WaitForConditionError::StreamEnded)
        );
        assert!(state.is_empty());
    }

    pub(crate) async fn handle_open_controller(
        mut request_stream: fnet_filter::ControlRequestStream,
    ) -> fnet_filter::NamespaceControllerRequestStream {
        let (id, request, _control_handle) = request_stream
            .next()
            .await
            .expect("client should open controller")
            .expect("request should not error")
            .into_open_controller()
            .expect("client should open controller");
        let (stream, control_handle) = request.into_stream_and_control_handle();
        control_handle.send_on_id_assigned(&id).expect("send assigned ID");

        stream
    }

    pub(crate) async fn handle_push_changes(
        stream: &mut fnet_filter::NamespaceControllerRequestStream,
        push_changes_result: fnet_filter::ChangeValidationResult,
    ) {
        let (_changes, responder) = stream
            .next()
            .await
            .expect("client should push changes")
            .expect("request should not error")
            .into_push_changes()
            .expect("client should push changes");
        responder.send(push_changes_result).expect("send empty batch");
    }

    pub(crate) async fn handle_commit(
        stream: &mut fnet_filter::NamespaceControllerRequestStream,
        commit_result: fnet_filter::CommitResult,
    ) {
        let (_options, responder) = stream
            .next()
            .await
            .expect("client should commit")
            .expect("request should not error")
            .into_commit()
            .expect("client should commit");
        responder.send(commit_result).expect("send commit result");
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn controller_push_changes_reports_invalid_change() {
        let (control, request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnet_filter::ControlMarker>().unwrap();
        let push_invalid_change = async {
            let mut controller = Controller::new(&control, &ControllerId(String::from("test")))
                .await
                .expect("create controller");
            let result = controller
                .push_changes(vec![
                    Change::Create(test_resource()),
                    Change::Create(invalid_resource()),
                    Change::Remove(test_resource_id()),
                ])
                .await;
            assert_matches!(
                result,
                Err(PushChangesError::ErrorOnChange(errors)) if errors == vec![(
                    Change::Create(invalid_resource()),
                    ChangeValidationError::InvalidPortMatcher
                )]
            );
        };

        let handle_controller = async {
            let mut stream = handle_open_controller(request_stream).await;
            handle_push_changes(
                &mut stream,
                fnet_filter::ChangeValidationResult::ErrorOnChange(vec![
                    fnet_filter::ChangeValidationError::Ok,
                    fnet_filter::ChangeValidationError::InvalidPortMatcher,
                    fnet_filter::ChangeValidationError::NotReached,
                ]),
            )
            .await;
        };

        let ((), ()) = futures::future::join(push_invalid_change, handle_controller).await;
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn controller_commit_reports_invalid_change() {
        let (control, request_stream) =
            fidl::endpoints::create_proxy_and_stream::<fnet_filter::ControlMarker>().unwrap();
        let commit_invalid_change = async {
            let mut controller = Controller::new(&control, &ControllerId(String::from("test")))
                .await
                .expect("create controller");
            controller
                .push_changes(vec![
                    Change::Create(test_resource()),
                    Change::Remove(unknown_resource_id()),
                    Change::Remove(test_resource_id()),
                ])
                .await
                .expect("push changes");
            let result = controller.commit().await;
            assert_matches!(
                result,
                Err(CommitError::ErrorOnChange(errors)) if errors == vec![(
                    Change::Remove(unknown_resource_id()),
                    ChangeCommitError::NamespaceNotFound,
                )]
            );
        };
        let handle_controller = async {
            let mut stream = handle_open_controller(request_stream).await;
            handle_push_changes(
                &mut stream,
                fnet_filter::ChangeValidationResult::Ok(fnet_filter::Empty {}),
            )
            .await;
            handle_commit(
                &mut stream,
                fnet_filter::CommitResult::ErrorOnChange(vec![
                    fnet_filter::CommitError::Ok,
                    fnet_filter::CommitError::NamespaceNotFound,
                    fnet_filter::CommitError::Ok,
                ]),
            )
            .await;
        };
        let ((), ()) = futures::future::join(commit_invalid_change, handle_controller).await;
    }
}
