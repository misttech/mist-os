// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    cm_fidl_validator,
    cm_rust_derive::{
        CapabilityDeclCommon, ExposeDeclCommon, ExposeDeclCommonAlwaysRequired, FidlDecl,
        OfferDeclCommon, OfferDeclCommonNoAvailability, UseDeclCommon,
    },
    cm_types, fidl_fuchsia_component_config as fconfig, fidl_fuchsia_component_decl as fdecl,
    fidl_fuchsia_data as fdata, fidl_fuchsia_io as fio, fidl_fuchsia_process as fprocess,
    flyweights::FlyStr,
    from_enum::FromEnum,
    lazy_static::lazy_static,
    std::collections::hash_map::Entry,
    std::collections::{BTreeMap, HashMap},
    std::convert::{From, TryFrom},
    std::fmt,
    std::hash::Hash,
    std::path::PathBuf,
    std::str::FromStr,
    thiserror::Error,
};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[cfg(feature = "serde")]
mod serde_ext;

lazy_static! {
    static ref DATA_TYPENAME: CapabilityName = CapabilityName::from("Data");
    static ref CACHE_TYPENAME: CapabilityName = CapabilityName::from("Cache");
    static ref META_TYPENAME: CapabilityName = CapabilityName::from("Meta");
}

/// Converts a fidl object into its corresponding native representation.
pub trait FidlIntoNative<T> {
    fn fidl_into_native(self) -> T;
}

impl<Native, Fidl> FidlIntoNative<Vec<Native>> for Vec<Fidl>
where
    Fidl: FidlIntoNative<Native>,
{
    fn fidl_into_native(self) -> Vec<Native> {
        self.into_iter().map(|s| s.fidl_into_native()).collect()
    }
}

pub trait NativeIntoFidl<T> {
    fn native_into_fidl(self) -> T;
}

impl<Native, Fidl> NativeIntoFidl<Vec<Fidl>> for Vec<Native>
where
    Native: NativeIntoFidl<Fidl>,
{
    fn native_into_fidl(self) -> Vec<Fidl> {
        self.into_iter().map(|s| s.native_into_fidl()).collect()
    }
}

/// Generates `FidlIntoNative` and `NativeIntoFidl` implementations that leaves the input unchanged.
macro_rules! fidl_translations_identical {
    ($into_type:ty) => {
        impl FidlIntoNative<$into_type> for $into_type {
            fn fidl_into_native(self) -> $into_type {
                self
            }
        }
        impl NativeIntoFidl<$into_type> for $into_type {
            fn native_into_fidl(self) -> Self {
                self
            }
        }
    };
}

/// Generates `FidlIntoNative` and `NativeIntoFidl` implementations that
/// delegate to existing `Into` implementations.
macro_rules! fidl_translations_from_into {
    ($native_type:ty, $fidl_type:ty) => {
        impl FidlIntoNative<$native_type> for $fidl_type {
            fn fidl_into_native(self) -> $native_type {
                self.into()
            }
        }
        impl NativeIntoFidl<$fidl_type> for $native_type {
            fn native_into_fidl(self) -> $fidl_type {
                self.into()
            }
        }
    };
}

/// Generates `FidlIntoNative` and `NativeIntoFidl` implementations for
/// an symmetrical enum types.
/// `fidl_type` should be the FIDL type while `native_type` should be
/// the Rust native type defined elsewhere in this file.
/// Each field of the enums must be provided in the `variant` fieldset.
macro_rules! fidl_translations_symmetrical_enums {
($fidl_type:ty , $native_type:ty, $($variant: ident),*) => {
        impl FidlIntoNative<$native_type> for $fidl_type {
            fn fidl_into_native(self) -> $native_type {
                match self {
                    $( <$fidl_type>::$variant => <$native_type>::$variant,  )*
                }
            }
        }
        impl NativeIntoFidl<$fidl_type> for $native_type {
            fn native_into_fidl(self) -> $fidl_type {
                match self {
                    $( <$native_type>::$variant => <$fidl_type>::$variant,  )*
                }
            }
        }
    };
}
#[derive(FidlDecl, Debug, Clone, PartialEq, Default)]
#[fidl_decl(fidl_table = "fdecl::Component")]
pub struct ComponentDecl {
    pub program: Option<ProgramDecl>,
    pub uses: Vec<UseDecl>,
    pub exposes: Vec<ExposeDecl>,
    pub offers: Vec<OfferDecl>,
    pub capabilities: Vec<CapabilityDecl>,
    pub children: Vec<ChildDecl>,
    pub collections: Vec<CollectionDecl>,
    pub facets: Option<fdata::Dictionary>,
    pub environments: Vec<EnvironmentDecl>,
    pub config: Option<ConfigDecl>,
}

impl ComponentDecl {
    /// Returns the runner used by this component, or `None` if this is a non-executable component.
    pub fn get_runner(&self) -> Option<&CapabilityName> {
        self.program.as_ref().and_then(|p| p.runner.as_ref())
    }

    /// Returns the `StorageDecl` corresponding to `storage_name`.
    pub fn find_storage_source<'a>(
        &'a self,
        storage_name: &CapabilityName,
    ) -> Option<&'a StorageDecl> {
        self.capabilities.iter().find_map(|c| match c {
            CapabilityDecl::Storage(s) if &s.name == storage_name => Some(s),
            _ => None,
        })
    }

    /// Returns the `ProtocolDecl` corresponding to `protocol_name`.
    pub fn find_protocol_source<'a>(
        &'a self,
        protocol_name: &CapabilityName,
    ) -> Option<&'a ProtocolDecl> {
        self.capabilities.iter().find_map(|c| match c {
            CapabilityDecl::Protocol(r) if &r.name == protocol_name => Some(r),
            _ => None,
        })
    }

    /// Returns the `DirectoryDecl` corresponding to `directory_name`.
    pub fn find_directory_source<'a>(
        &'a self,
        directory_name: &CapabilityName,
    ) -> Option<&'a DirectoryDecl> {
        self.capabilities.iter().find_map(|c| match c {
            CapabilityDecl::Directory(r) if &r.name == directory_name => Some(r),
            _ => None,
        })
    }

    /// Returns the `RunnerDecl` corresponding to `runner_name`.
    pub fn find_runner_source<'a>(
        &'a self,
        runner_name: &CapabilityName,
    ) -> Option<&'a RunnerDecl> {
        self.capabilities.iter().find_map(|c| match c {
            CapabilityDecl::Runner(r) if &r.name == runner_name => Some(r),
            _ => None,
        })
    }

    /// Returns the `ResolverDecl` corresponding to `resolver_name`.
    pub fn find_resolver_source<'a>(
        &'a self,
        resolver_name: &CapabilityName,
    ) -> Option<&'a ResolverDecl> {
        self.capabilities.iter().find_map(|c| match c {
            CapabilityDecl::Resolver(r) if &r.name == resolver_name => Some(r),
            _ => None,
        })
    }

    /// Returns the `CollectionDecl` corresponding to `collection_name`.
    pub fn find_collection<'a>(&'a self, collection_name: &str) -> Option<&'a CollectionDecl> {
        self.collections.iter().find(|c| c.name == collection_name)
    }

    /// Indicates whether the capability specified by `target_name` is exposed to the framework.
    pub fn is_protocol_exposed_to_framework(&self, in_target_name: &CapabilityName) -> bool {
        self.exposes.iter().any(|expose| match expose {
            ExposeDecl::Protocol(ExposeProtocolDecl { target, target_name, .. })
                if target == &ExposeTarget::Framework =>
            {
                target_name == in_target_name
            }
            _ => false,
        })
    }

    /// Indicates whether the capability specified by `source_name` is requested.
    pub fn uses_protocol(&self, source_name: &CapabilityName) -> bool {
        self.uses.iter().any(|use_decl| match use_decl {
            UseDecl::Protocol(ls) => &ls.source_name == source_name,
            _ => false,
        })
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Availability {
    Required,
    Optional,
    SameAsTarget,
    Transitional,
}

// TODO(dgonyeo): remove this once we've soft migrated to the availability field being required.
impl Default for Availability {
    fn default() -> Self {
        Availability::Required
    }
}

fidl_translations_symmetrical_enums!(
    fdecl::Availability,
    Availability,
    Required,
    Optional,
    SameAsTarget,
    Transitional
);

#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(tag = "type", rename_all = "snake_case")
)]
#[derive(FidlDecl, FromEnum, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_union = "fdecl::Use")]
pub enum UseDecl {
    Service(UseServiceDecl),
    Protocol(UseProtocolDecl),
    Directory(UseDirectoryDecl),
    Storage(UseStorageDecl),
    EventStream(UseEventStreamDecl),
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, UseDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::UseService")]
pub struct UseServiceDecl {
    pub source: UseSource,
    pub source_name: CapabilityName,
    pub target_path: CapabilityPath,
    pub dependency_type: DependencyType,
    #[fidl_decl(default)]
    pub availability: Availability,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, UseDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::UseProtocol")]
pub struct UseProtocolDecl {
    pub source: UseSource,
    pub source_name: CapabilityName,
    pub target_path: CapabilityPath,
    pub dependency_type: DependencyType,
    #[fidl_decl(default)]
    pub availability: Availability,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, UseDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::UseDirectory")]
pub struct UseDirectoryDecl {
    pub source: UseSource,
    pub source_name: CapabilityName,
    pub target_path: CapabilityPath,

    #[cfg_attr(
        feature = "serde",
        serde(
            deserialize_with = "serde_ext::deserialize_fio_operations",
            serialize_with = "serde_ext::serialize_fio_operations"
        )
    )]
    pub rights: fio::Operations,

    pub subdir: Option<PathBuf>,
    pub dependency_type: DependencyType,
    #[fidl_decl(default)]
    pub availability: Availability,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::UseStorage")]
pub struct UseStorageDecl {
    pub source_name: CapabilityName,
    pub target_path: CapabilityPath,
    #[fidl_decl(default)]
    pub availability: Availability,
}

impl SourceName for UseStorageDecl {
    fn source_name(&self) -> &CapabilityName {
        &self.source_name
    }
}

impl UseDeclCommon for UseStorageDecl {
    fn source(&self) -> &UseSource {
        &UseSource::Parent
    }

    fn availability(&self) -> &Availability {
        &self.availability
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::EventSubscription")]
pub struct EventSubscription {
    pub event_name: String,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, UseDeclCommon, Debug, Clone, PartialEq, Eq, Hash)]
#[fidl_decl(fidl_table = "fdecl::UseEventStream")]
pub struct UseEventStreamDecl {
    pub source_name: CapabilityName,
    pub source: UseSource,
    pub scope: Option<Vec<EventScope>>,
    pub target_path: CapabilityPath,
    pub filter: Option<BTreeMap<String, DictionaryValue>>,
    #[fidl_decl(default)]
    pub availability: Availability,
}

#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(tag = "type", rename_all = "snake_case")
)]
#[derive(FidlDecl, FromEnum, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_union = "fdecl::Offer")]
pub enum OfferDecl {
    Service(OfferServiceDecl),
    Protocol(OfferProtocolDecl),
    Directory(OfferDirectoryDecl),
    Storage(OfferStorageDecl),
    Runner(OfferRunnerDecl),
    Resolver(OfferResolverDecl),
    EventStream(OfferEventStreamDecl),
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, OfferDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::OfferEventStream")]
pub struct OfferEventStreamDecl {
    pub source: OfferSource,
    pub scope: Option<Vec<EventScope>>,
    pub filter: Option<HashMap<String, DictionaryValue>>,
    pub source_name: CapabilityName,
    pub target: OfferTarget,
    pub target_name: CapabilityName,
    #[fidl_decl(default)]
    pub availability: Availability,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameMapping {
    pub source_name: String,
    pub target_name: String,
}

pub fn name_mappings_to_map(name_mappings: Vec<NameMapping>) -> HashMap<String, Vec<String>> {
    let mut m = HashMap::<String, Vec<String>>::new();
    for mapping in name_mappings.iter() {
        match m.entry(mapping.clone().source_name) {
            Entry::Occupied(mut entry) => {
                let val = entry.get_mut();
                val.push(mapping.clone().target_name);
            }
            Entry::Vacant(entry) => {
                entry.insert(vec![mapping.clone().target_name]);
            }
        }
    }
    m
}

impl NativeIntoFidl<fdecl::NameMapping> for NameMapping {
    fn native_into_fidl(self) -> fdecl::NameMapping {
        fdecl::NameMapping { source_name: self.source_name, target_name: self.target_name }
    }
}

impl FidlIntoNative<NameMapping> for fdecl::NameMapping {
    fn fidl_into_native(self) -> NameMapping {
        NameMapping { source_name: self.source_name, target_name: self.target_name }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, OfferDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::OfferService")]
pub struct OfferServiceDecl {
    pub source: OfferSource,
    pub source_name: CapabilityName,
    pub target: OfferTarget,
    pub target_name: CapabilityName,
    pub source_instance_filter: Option<Vec<String>>,
    pub renamed_instances: Option<Vec<NameMapping>>,
    #[fidl_decl(default)]
    pub availability: Availability,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, OfferDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::OfferProtocol")]
pub struct OfferProtocolDecl {
    pub source: OfferSource,
    pub source_name: CapabilityName,
    pub target: OfferTarget,
    pub target_name: CapabilityName,
    pub dependency_type: DependencyType,
    #[fidl_decl(default)]
    pub availability: Availability,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, OfferDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::OfferDirectory")]
pub struct OfferDirectoryDecl {
    pub source: OfferSource,
    pub source_name: CapabilityName,
    pub target: OfferTarget,
    pub target_name: CapabilityName,
    pub dependency_type: DependencyType,

    #[cfg_attr(
        feature = "serde",
        serde(
            deserialize_with = "serde_ext::deserialize_opt_fio_operations",
            serialize_with = "serde_ext::serialize_opt_fio_operations"
        )
    )]
    pub rights: Option<fio::Operations>,

    pub subdir: Option<PathBuf>,
    #[fidl_decl(default)]
    pub availability: Availability,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, OfferDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::OfferStorage")]
pub struct OfferStorageDecl {
    pub source: OfferSource,
    pub source_name: CapabilityName,
    pub target: OfferTarget,
    pub target_name: CapabilityName,
    #[fidl_decl(default)]
    pub availability: Availability,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, OfferDeclCommonNoAvailability, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::OfferRunner")]
pub struct OfferRunnerDecl {
    pub source: OfferSource,
    pub source_name: CapabilityName,
    pub target: OfferTarget,
    pub target_name: CapabilityName,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, OfferDeclCommonNoAvailability, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::OfferResolver")]
pub struct OfferResolverDecl {
    pub source: OfferSource,
    pub source_name: CapabilityName,
    pub target: OfferTarget,
    pub target_name: CapabilityName,
}

impl SourceName for OfferDecl {
    fn source_name(&self) -> &CapabilityName {
        match &self {
            OfferDecl::Service(o) => o.source_name(),
            OfferDecl::Protocol(o) => o.source_name(),
            OfferDecl::Directory(o) => o.source_name(),
            OfferDecl::Storage(o) => o.source_name(),
            OfferDecl::Runner(o) => o.source_name(),
            OfferDecl::Resolver(o) => o.source_name(),
            OfferDecl::EventStream(o) => o.source_name(),
        }
    }
}

impl UseDeclCommon for UseDecl {
    fn source(&self) -> &UseSource {
        match &self {
            UseDecl::Service(u) => u.source(),
            UseDecl::Protocol(u) => u.source(),
            UseDecl::Directory(u) => u.source(),
            UseDecl::Storage(u) => u.source(),
            UseDecl::EventStream(u) => u.source(),
        }
    }

    fn availability(&self) -> &Availability {
        match &self {
            UseDecl::Service(u) => u.availability(),
            UseDecl::Protocol(u) => u.availability(),
            UseDecl::Directory(u) => u.availability(),
            UseDecl::Storage(u) => u.availability(),
            UseDecl::EventStream(u) => u.availability(),
        }
    }
}

impl OfferDeclCommon for OfferDecl {
    fn target_name(&self) -> &CapabilityName {
        match &self {
            OfferDecl::Service(o) => o.target_name(),
            OfferDecl::Protocol(o) => o.target_name(),
            OfferDecl::Directory(o) => o.target_name(),
            OfferDecl::Storage(o) => o.target_name(),
            OfferDecl::Runner(o) => o.target_name(),
            OfferDecl::Resolver(o) => o.target_name(),
            OfferDecl::EventStream(o) => o.target_name(),
        }
    }

    fn target(&self) -> &OfferTarget {
        match &self {
            OfferDecl::Service(o) => o.target(),
            OfferDecl::Protocol(o) => o.target(),
            OfferDecl::Directory(o) => o.target(),
            OfferDecl::Storage(o) => o.target(),
            OfferDecl::Runner(o) => o.target(),
            OfferDecl::Resolver(o) => o.target(),
            OfferDecl::EventStream(o) => o.target(),
        }
    }

    fn source(&self) -> &OfferSource {
        match &self {
            OfferDecl::Service(o) => o.source(),
            OfferDecl::Protocol(o) => o.source(),
            OfferDecl::Directory(o) => o.source(),
            OfferDecl::Storage(o) => o.source(),
            OfferDecl::Runner(o) => o.source(),
            OfferDecl::Resolver(o) => o.source(),
            OfferDecl::EventStream(o) => o.source(),
        }
    }

    fn availability(&self) -> Option<&Availability> {
        match &self {
            OfferDecl::Service(o) => o.availability(),
            OfferDecl::Protocol(o) => o.availability(),
            OfferDecl::Directory(o) => o.availability(),
            OfferDecl::Storage(o) => o.availability(),
            OfferDecl::Runner(o) => o.availability(),
            OfferDecl::Resolver(o) => o.availability(),
            OfferDecl::EventStream(o) => o.availability(),
        }
    }
}

#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(tag = "type", rename_all = "snake_case")
)]
#[derive(FidlDecl, FromEnum, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_union = "fdecl::Expose")]
pub enum ExposeDecl {
    Service(ExposeServiceDecl),
    Protocol(ExposeProtocolDecl),
    Directory(ExposeDirectoryDecl),
    Runner(ExposeRunnerDecl),
    Resolver(ExposeResolverDecl),
}

impl SourceName for ExposeDecl {
    fn source_name(&self) -> &CapabilityName {
        match self {
            Self::Service(e) => e.source_name(),
            Self::Protocol(e) => e.source_name(),
            Self::Directory(e) => e.source_name(),
            Self::Runner(e) => e.source_name(),
            Self::Resolver(e) => e.source_name(),
        }
    }
}

impl ExposeDeclCommon for ExposeDecl {
    fn source(&self) -> &ExposeSource {
        match self {
            Self::Service(e) => e.source(),
            Self::Protocol(e) => e.source(),
            Self::Directory(e) => e.source(),
            Self::Runner(e) => e.source(),
            Self::Resolver(e) => e.source(),
        }
    }

    fn target(&self) -> &ExposeTarget {
        match self {
            Self::Service(e) => e.target(),
            Self::Protocol(e) => e.target(),
            Self::Directory(e) => e.target(),
            Self::Runner(e) => e.target(),
            Self::Resolver(e) => e.target(),
        }
    }

    fn target_name(&self) -> &CapabilityName {
        match self {
            Self::Service(e) => e.target_name(),
            Self::Protocol(e) => e.target_name(),
            Self::Directory(e) => e.target_name(),
            Self::Runner(e) => e.target_name(),
            Self::Resolver(e) => e.target_name(),
        }
    }

    fn availability(&self) -> &Availability {
        match self {
            Self::Service(e) => e.availability(),
            Self::Protocol(e) => e.availability(),
            Self::Directory(e) => e.availability(),
            Self::Runner(e) => e.availability(),
            Self::Resolver(e) => e.availability(),
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, ExposeDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::ExposeService")]
pub struct ExposeServiceDecl {
    pub source: ExposeSource,
    pub source_name: CapabilityName,
    pub target: ExposeTarget,
    pub target_name: CapabilityName,
    #[fidl_decl(default)]
    pub availability: Availability,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, ExposeDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::ExposeProtocol")]
pub struct ExposeProtocolDecl {
    pub source: ExposeSource,
    pub source_name: CapabilityName,
    pub target: ExposeTarget,
    pub target_name: CapabilityName,
    #[fidl_decl(default)]
    pub availability: Availability,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, ExposeDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::ExposeDirectory")]
pub struct ExposeDirectoryDecl {
    pub source: ExposeSource,
    pub source_name: CapabilityName,
    pub target: ExposeTarget,
    pub target_name: CapabilityName,

    #[cfg_attr(
        feature = "serde",
        serde(
            deserialize_with = "serde_ext::deserialize_opt_fio_operations",
            serialize_with = "serde_ext::serialize_opt_fio_operations"
        )
    )]
    pub rights: Option<fio::Operations>,

    pub subdir: Option<PathBuf>,

    #[fidl_decl(default)]
    pub availability: Availability,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, ExposeDeclCommonAlwaysRequired, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::ExposeRunner")]
pub struct ExposeRunnerDecl {
    pub source: ExposeSource,
    pub source_name: CapabilityName,
    pub target: ExposeTarget,
    pub target_name: CapabilityName,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, ExposeDeclCommonAlwaysRequired, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::ExposeResolver")]
pub struct ExposeResolverDecl {
    pub source: ExposeSource,
    pub source_name: CapabilityName,
    pub target: ExposeTarget,
    pub target_name: CapabilityName,
}

#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(tag = "type", rename_all = "snake_case")
)]
#[derive(FidlDecl, FromEnum, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_union = "fdecl::Capability")]
pub enum CapabilityDecl {
    Service(ServiceDecl),
    Protocol(ProtocolDecl),
    Directory(DirectoryDecl),
    Storage(StorageDecl),
    Runner(RunnerDecl),
    Resolver(ResolverDecl),
    EventStream(EventStreamDecl),
}

impl CapabilityDeclCommon for CapabilityDecl {
    fn name(&self) -> &CapabilityName {
        match self {
            Self::Service(c) => c.name(),
            Self::Protocol(c) => c.name(),
            Self::Directory(c) => c.name(),
            Self::Storage(c) => c.name(),
            Self::Runner(c) => c.name(),
            Self::Resolver(c) => c.name(),
            Self::EventStream(c) => c.name(),
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, CapabilityDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::Service")]
pub struct ServiceDecl {
    pub name: CapabilityName,
    pub source_path: Option<CapabilityPath>,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, CapabilityDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::Protocol")]
pub struct ProtocolDecl {
    pub name: CapabilityName,
    pub source_path: Option<CapabilityPath>,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, CapabilityDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::Directory")]
pub struct DirectoryDecl {
    pub name: CapabilityName,
    pub source_path: Option<CapabilityPath>,

    #[cfg_attr(
        feature = "serde",
        serde(
            deserialize_with = "serde_ext::deserialize_fio_operations",
            serialize_with = "serde_ext::serialize_fio_operations"
        )
    )]
    pub rights: fio::Operations,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, CapabilityDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::Storage")]
pub struct StorageDecl {
    pub name: CapabilityName,
    pub source: StorageDirectorySource,
    pub backing_dir: CapabilityName,
    pub subdir: Option<PathBuf>,
    #[cfg_attr(feature = "serde", serde(with = "serde_ext::StorageId"))]
    pub storage_id: fdecl::StorageId,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, CapabilityDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::Runner")]
pub struct RunnerDecl {
    pub name: CapabilityName,
    pub source_path: Option<CapabilityPath>,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, CapabilityDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::Resolver")]
pub struct ResolverDecl {
    pub name: CapabilityName,
    pub source_path: Option<CapabilityPath>,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, CapabilityDeclCommon, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::EventStream")]
pub struct EventStreamDecl {
    pub name: CapabilityName,
}

impl CapabilityDecl {
    pub fn name(&self) -> &CapabilityName {
        match self {
            CapabilityDecl::Directory(decl) => decl.name(),
            CapabilityDecl::Protocol(decl) => decl.name(),
            CapabilityDecl::Resolver(decl) => decl.name(),
            CapabilityDecl::Runner(decl) => decl.name(),
            CapabilityDecl::Service(decl) => decl.name(),
            CapabilityDecl::Storage(decl) => decl.name(),
            CapabilityDecl::EventStream(decl) => decl.name(),
        }
    }

    pub fn path(&self) -> Option<&CapabilityPath> {
        match self {
            CapabilityDecl::Directory(decl) => decl.source_path.as_ref(),
            CapabilityDecl::Protocol(decl) => decl.source_path.as_ref(),
            CapabilityDecl::Resolver(decl) => decl.source_path.as_ref(),
            CapabilityDecl::Runner(decl) => decl.source_path.as_ref(),
            CapabilityDecl::Service(decl) => decl.source_path.as_ref(),
            CapabilityDecl::Storage(_) => None,
            CapabilityDecl::EventStream(_) => None,
        }
    }
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::Child")]
pub struct ChildDecl {
    pub name: String,
    pub url: String,
    pub startup: fdecl::StartupMode,
    pub on_terminate: Option<fdecl::OnTerminate>,
    pub environment: Option<String>,
    pub config_overrides: Option<Vec<ConfigOverride>>,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ChildRef {
    pub name: FlyStr,
    pub collection: Option<FlyStr>,
}

impl std::fmt::Display for ChildRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(collection) = &self.collection {
            write!(f, "{}:{}", collection, self.name)
        } else {
            write!(f, "{}", self.name)
        }
    }
}

impl FidlIntoNative<ChildRef> for fdecl::ChildRef {
    fn fidl_into_native(self) -> ChildRef {
        ChildRef { name: self.name.into(), collection: self.collection.map(Into::into) }
    }
}

impl NativeIntoFidl<fdecl::ChildRef> for ChildRef {
    fn native_into_fidl(self) -> fdecl::ChildRef {
        fdecl::ChildRef { name: self.name.into(), collection: self.collection.map(Into::into) }
    }
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::Collection")]
pub struct CollectionDecl {
    pub name: String,
    pub durability: fdecl::Durability,
    pub environment: Option<String>,

    #[fidl_decl(default)]
    pub allowed_offers: cm_types::AllowedOffers,
    #[fidl_decl(default)]
    pub allow_long_names: bool,

    pub persistent_storage: Option<bool>,
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::Environment")]
pub struct EnvironmentDecl {
    pub name: String,
    pub extends: fdecl::EnvironmentExtends,
    pub runners: Vec<RunnerRegistration>,
    pub resolvers: Vec<ResolverRegistration>,
    pub debug_capabilities: Vec<DebugRegistration>,
    pub stop_timeout_ms: Option<u32>,
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[fidl_decl(fidl_table = "fdecl::ConfigOverride")]
pub struct ConfigOverride {
    pub key: String,
    pub value: ConfigValue,
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[fidl_decl(fidl_table = "fdecl::ConfigSchema")]
pub struct ConfigDecl {
    pub fields: Vec<ConfigField>,
    pub checksum: ConfigChecksum,
    pub value_source: ConfigValueSource,
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[fidl_decl(fidl_union = "fdecl::ConfigChecksum")]
pub enum ConfigChecksum {
    Sha256([u8; 32]),
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[fidl_decl(fidl_union = "fdecl::ConfigValueSource")]
pub enum ConfigValueSource {
    PackagePath(String),
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[fidl_decl(fidl_table = "fdecl::ConfigField")]
pub struct ConfigField {
    pub key: String,
    pub type_: ConfigValueType,

    // This field will not be present in compiled manifests which predate F12.
    #[fidl_decl(default)]
    pub mutability: ConfigMutability,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigNestedValueType {
    Bool,
    Uint8,
    Int8,
    Uint16,
    Int16,
    Uint32,
    Int32,
    Uint64,
    Int64,
    String { max_size: u32 },
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigValueType {
    Bool,
    Uint8,
    Int8,
    Uint16,
    Int16,
    Uint32,
    Int32,
    Uint64,
    Int64,
    String { max_size: u32 },
    Vector { nested_type: ConfigNestedValueType, max_count: u32 },
}

impl FidlIntoNative<ConfigNestedValueType> for fdecl::ConfigType {
    fn fidl_into_native(mut self) -> ConfigNestedValueType {
        match self.layout {
            fdecl::ConfigTypeLayout::Bool => ConfigNestedValueType::Bool,
            fdecl::ConfigTypeLayout::Uint8 => ConfigNestedValueType::Uint8,
            fdecl::ConfigTypeLayout::Uint16 => ConfigNestedValueType::Uint16,
            fdecl::ConfigTypeLayout::Uint32 => ConfigNestedValueType::Uint32,
            fdecl::ConfigTypeLayout::Uint64 => ConfigNestedValueType::Uint64,
            fdecl::ConfigTypeLayout::Int8 => ConfigNestedValueType::Int8,
            fdecl::ConfigTypeLayout::Int16 => ConfigNestedValueType::Int16,
            fdecl::ConfigTypeLayout::Int32 => ConfigNestedValueType::Int32,
            fdecl::ConfigTypeLayout::Int64 => ConfigNestedValueType::Int64,
            fdecl::ConfigTypeLayout::String => {
                let max_size =
                    if let fdecl::LayoutConstraint::MaxSize(s) = self.constraints.remove(0) {
                        s
                    } else {
                        panic!("Unexpected constraint on String layout type for config field");
                    };
                ConfigNestedValueType::String { max_size }
            }
            fdecl::ConfigTypeLayout::Vector => {
                panic!("Nested vectors are not supported in structured config")
            }
            fdecl::ConfigTypeLayoutUnknown!() => panic!("Unknown layout type for config field"),
        }
    }
}

impl NativeIntoFidl<fdecl::ConfigType> for ConfigNestedValueType {
    fn native_into_fidl(self) -> fdecl::ConfigType {
        let layout = match self {
            ConfigNestedValueType::Bool => fdecl::ConfigTypeLayout::Bool,
            ConfigNestedValueType::Uint8 => fdecl::ConfigTypeLayout::Uint8,
            ConfigNestedValueType::Uint16 => fdecl::ConfigTypeLayout::Uint16,
            ConfigNestedValueType::Uint32 => fdecl::ConfigTypeLayout::Uint32,
            ConfigNestedValueType::Uint64 => fdecl::ConfigTypeLayout::Uint64,
            ConfigNestedValueType::Int8 => fdecl::ConfigTypeLayout::Int8,
            ConfigNestedValueType::Int16 => fdecl::ConfigTypeLayout::Int16,
            ConfigNestedValueType::Int32 => fdecl::ConfigTypeLayout::Int32,
            ConfigNestedValueType::Int64 => fdecl::ConfigTypeLayout::Int64,
            ConfigNestedValueType::String { .. } => fdecl::ConfigTypeLayout::String,
        };
        let constraints = match self {
            ConfigNestedValueType::String { max_size } => {
                vec![fdecl::LayoutConstraint::MaxSize(max_size)]
            }
            _ => vec![],
        };
        fdecl::ConfigType { layout, constraints, parameters: Some(vec![]) }
    }
}

impl FidlIntoNative<ConfigValueType> for fdecl::ConfigType {
    fn fidl_into_native(mut self) -> ConfigValueType {
        match self.layout {
            fdecl::ConfigTypeLayout::Bool => ConfigValueType::Bool,
            fdecl::ConfigTypeLayout::Uint8 => ConfigValueType::Uint8,
            fdecl::ConfigTypeLayout::Uint16 => ConfigValueType::Uint16,
            fdecl::ConfigTypeLayout::Uint32 => ConfigValueType::Uint32,
            fdecl::ConfigTypeLayout::Uint64 => ConfigValueType::Uint64,
            fdecl::ConfigTypeLayout::Int8 => ConfigValueType::Int8,
            fdecl::ConfigTypeLayout::Int16 => ConfigValueType::Int16,
            fdecl::ConfigTypeLayout::Int32 => ConfigValueType::Int32,
            fdecl::ConfigTypeLayout::Int64 => ConfigValueType::Int64,
            fdecl::ConfigTypeLayout::String => {
                let max_size = if let fdecl::LayoutConstraint::MaxSize(s) =
                    self.constraints.remove(0)
                {
                    s
                } else {
                    panic!("Unexpected constraint on String layout type for config field. Expected MaxSize.");
                };
                ConfigValueType::String { max_size }
            }
            fdecl::ConfigTypeLayout::Vector => {
                let max_count = if let fdecl::LayoutConstraint::MaxSize(c) =
                    self.constraints.remove(0)
                {
                    c
                } else {
                    panic!("Unexpected constraint on Vector layout type for config field. Expected MaxSize.");
                };
                let mut parameters =
                    self.parameters.expect("Config field must have parameters set");
                let nested_type = if let fdecl::LayoutParameter::NestedType(nested_type) =
                    parameters.remove(0)
                {
                    nested_type.fidl_into_native()
                } else {
                    panic!("Unexpected parameter on Vector layout type for config field. Expected NestedType.");
                };
                ConfigValueType::Vector { max_count, nested_type }
            }
            fdecl::ConfigTypeLayoutUnknown!() => panic!("Unknown layout type for config field"),
        }
    }
}

impl NativeIntoFidl<fdecl::ConfigType> for ConfigValueType {
    fn native_into_fidl(self) -> fdecl::ConfigType {
        let layout = match self {
            ConfigValueType::Bool => fdecl::ConfigTypeLayout::Bool,
            ConfigValueType::Uint8 => fdecl::ConfigTypeLayout::Uint8,
            ConfigValueType::Uint16 => fdecl::ConfigTypeLayout::Uint16,
            ConfigValueType::Uint32 => fdecl::ConfigTypeLayout::Uint32,
            ConfigValueType::Uint64 => fdecl::ConfigTypeLayout::Uint64,
            ConfigValueType::Int8 => fdecl::ConfigTypeLayout::Int8,
            ConfigValueType::Int16 => fdecl::ConfigTypeLayout::Int16,
            ConfigValueType::Int32 => fdecl::ConfigTypeLayout::Int32,
            ConfigValueType::Int64 => fdecl::ConfigTypeLayout::Int64,
            ConfigValueType::String { .. } => fdecl::ConfigTypeLayout::String,
            ConfigValueType::Vector { .. } => fdecl::ConfigTypeLayout::Vector,
        };
        let (constraints, parameters) = match self {
            ConfigValueType::String { max_size } => {
                (vec![fdecl::LayoutConstraint::MaxSize(max_size)], vec![])
            }
            ConfigValueType::Vector { max_count, nested_type } => {
                let nested_type = nested_type.native_into_fidl();
                (
                    vec![fdecl::LayoutConstraint::MaxSize(max_count)],
                    vec![fdecl::LayoutParameter::NestedType(nested_type)],
                )
            }
            _ => (vec![], vec![]),
        };
        fdecl::ConfigType { layout, constraints, parameters: Some(parameters) }
    }
}

bitflags::bitflags! {
    #[derive(Default)]
    #[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
    // TODO(https://fxbug.dev/124335) uncomment once bitflags is updated
    // pub struct ConfigMutability: <fdecl::ConfigMutability as bitflags::BitFlags>::Bits {
    pub struct ConfigMutability: u32 {
        const PARENT = fdecl::ConfigMutability::PARENT.bits();
    }
}

impl NativeIntoFidl<fdecl::ConfigMutability> for ConfigMutability {
    fn native_into_fidl(self) -> fdecl::ConfigMutability {
        fdecl::ConfigMutability::from_bits_allow_unknown(self.bits())
    }
}

impl FidlIntoNative<ConfigMutability> for fdecl::ConfigMutability {
    fn fidl_into_native(self) -> ConfigMutability {
        // TODO(https://fxbug.dev/124335) remove unsafe once bitflags is updated
        // SAFETY: this called from_bits_retain in newer versions of bitflags and is safe
        unsafe { ConfigMutability::from_bits_unchecked(self.bits()) }
    }
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[fidl_decl(fidl_table = "fdecl::ConfigValuesData")]
pub struct ConfigValuesData {
    pub values: Vec<ConfigValueSpec>,
    pub checksum: ConfigChecksum,
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[fidl_decl(fidl_table = "fdecl::ConfigValueSpec")]
pub struct ConfigValueSpec {
    pub value: ConfigValue,
}

impl ConfigValueSpec {
    // TODO(https://fxbug.dev/126609) delete this once fuchsia.component.config is removed
    pub fn native_into_fidl_todo_fxb_126609(self) -> fconfig::ValueSpec {
        fidl_fuchsia_component_config::ValueSpec {
            value: match self.value {
                ConfigValue::Single(ConfigSingleValue::Bool(b)) => {
                    Some(fconfig::Value::Single(fconfig::SingleValue::Bool(b)))
                }
                ConfigValue::Single(ConfigSingleValue::Uint8(n)) => {
                    Some(fconfig::Value::Single(fconfig::SingleValue::Uint8(n)))
                }
                ConfigValue::Single(ConfigSingleValue::Uint16(n)) => {
                    Some(fconfig::Value::Single(fconfig::SingleValue::Uint16(n)))
                }
                ConfigValue::Single(ConfigSingleValue::Uint32(n)) => {
                    Some(fconfig::Value::Single(fconfig::SingleValue::Uint32(n)))
                }
                ConfigValue::Single(ConfigSingleValue::Uint64(n)) => {
                    Some(fconfig::Value::Single(fconfig::SingleValue::Uint64(n)))
                }
                ConfigValue::Single(ConfigSingleValue::Int8(n)) => {
                    Some(fconfig::Value::Single(fconfig::SingleValue::Int8(n)))
                }
                ConfigValue::Single(ConfigSingleValue::Int16(n)) => {
                    Some(fconfig::Value::Single(fconfig::SingleValue::Int16(n)))
                }
                ConfigValue::Single(ConfigSingleValue::Int32(n)) => {
                    Some(fconfig::Value::Single(fconfig::SingleValue::Int32(n)))
                }
                ConfigValue::Single(ConfigSingleValue::Int64(n)) => {
                    Some(fconfig::Value::Single(fconfig::SingleValue::Int64(n)))
                }
                ConfigValue::Single(ConfigSingleValue::String(n)) => {
                    Some(fconfig::Value::Single(fconfig::SingleValue::String(n)))
                }
                ConfigValue::Vector(ConfigVectorValue::BoolVector(b)) => {
                    Some(fconfig::Value::Vector(fconfig::VectorValue::BoolVector(b)))
                }
                ConfigValue::Vector(ConfigVectorValue::Uint8Vector(n)) => {
                    Some(fconfig::Value::Vector(fconfig::VectorValue::Uint8Vector(n)))
                }
                ConfigValue::Vector(ConfigVectorValue::Uint16Vector(n)) => {
                    Some(fconfig::Value::Vector(fconfig::VectorValue::Uint16Vector(n)))
                }
                ConfigValue::Vector(ConfigVectorValue::Uint32Vector(n)) => {
                    Some(fconfig::Value::Vector(fconfig::VectorValue::Uint32Vector(n)))
                }
                ConfigValue::Vector(ConfigVectorValue::Uint64Vector(n)) => {
                    Some(fconfig::Value::Vector(fconfig::VectorValue::Uint64Vector(n)))
                }
                ConfigValue::Vector(ConfigVectorValue::Int8Vector(n)) => {
                    Some(fconfig::Value::Vector(fconfig::VectorValue::Int8Vector(n)))
                }
                ConfigValue::Vector(ConfigVectorValue::Int16Vector(n)) => {
                    Some(fconfig::Value::Vector(fconfig::VectorValue::Int16Vector(n)))
                }
                ConfigValue::Vector(ConfigVectorValue::Int32Vector(n)) => {
                    Some(fconfig::Value::Vector(fconfig::VectorValue::Int32Vector(n)))
                }
                ConfigValue::Vector(ConfigVectorValue::Int64Vector(n)) => {
                    Some(fconfig::Value::Vector(fconfig::VectorValue::Int64Vector(n)))
                }
                ConfigValue::Vector(ConfigVectorValue::StringVector(n)) => {
                    Some(fconfig::Value::Vector(fconfig::VectorValue::StringVector(n)))
                }
            },
            ..Default::default()
        }
    }

    // TODO(https://fxbug.dev/126609) delete this once fuchsia.component.config is being removed
    pub fn fidl_into_native_todo_fxb_126609(spec: fconfig::ValueSpec) -> Self {
        Self {
            value: match spec.value {
                Some(fconfig::Value::Single(fconfig::SingleValue::Bool(b))) => {
                    ConfigValue::Single(ConfigSingleValue::Bool(b))
                }
                Some(fconfig::Value::Single(fconfig::SingleValue::Uint8(n))) => {
                    ConfigValue::Single(ConfigSingleValue::Uint8(n))
                }
                Some(fconfig::Value::Single(fconfig::SingleValue::Uint16(n))) => {
                    ConfigValue::Single(ConfigSingleValue::Uint16(n))
                }
                Some(fconfig::Value::Single(fconfig::SingleValue::Uint32(n))) => {
                    ConfigValue::Single(ConfigSingleValue::Uint32(n))
                }
                Some(fconfig::Value::Single(fconfig::SingleValue::Uint64(n))) => {
                    ConfigValue::Single(ConfigSingleValue::Uint64(n))
                }
                Some(fconfig::Value::Single(fconfig::SingleValue::Int8(n))) => {
                    ConfigValue::Single(ConfigSingleValue::Int8(n))
                }
                Some(fconfig::Value::Single(fconfig::SingleValue::Int16(n))) => {
                    ConfigValue::Single(ConfigSingleValue::Int16(n))
                }
                Some(fconfig::Value::Single(fconfig::SingleValue::Int32(n))) => {
                    ConfigValue::Single(ConfigSingleValue::Int32(n))
                }
                Some(fconfig::Value::Single(fconfig::SingleValue::Int64(n))) => {
                    ConfigValue::Single(ConfigSingleValue::Int64(n))
                }
                Some(fconfig::Value::Single(fconfig::SingleValue::String(n))) => {
                    ConfigValue::Single(ConfigSingleValue::String(n))
                }
                Some(fconfig::Value::Vector(fconfig::VectorValue::BoolVector(b))) => {
                    ConfigValue::Vector(ConfigVectorValue::BoolVector(b))
                }
                Some(fconfig::Value::Vector(fconfig::VectorValue::Uint8Vector(n))) => {
                    ConfigValue::Vector(ConfigVectorValue::Uint8Vector(n))
                }
                Some(fconfig::Value::Vector(fconfig::VectorValue::Uint16Vector(n))) => {
                    ConfigValue::Vector(ConfigVectorValue::Uint16Vector(n))
                }
                Some(fconfig::Value::Vector(fconfig::VectorValue::Uint32Vector(n))) => {
                    ConfigValue::Vector(ConfigVectorValue::Uint32Vector(n))
                }
                Some(fconfig::Value::Vector(fconfig::VectorValue::Uint64Vector(n))) => {
                    ConfigValue::Vector(ConfigVectorValue::Uint64Vector(n))
                }
                Some(fconfig::Value::Vector(fconfig::VectorValue::Int8Vector(n))) => {
                    ConfigValue::Vector(ConfigVectorValue::Int8Vector(n))
                }
                Some(fconfig::Value::Vector(fconfig::VectorValue::Int16Vector(n))) => {
                    ConfigValue::Vector(ConfigVectorValue::Int16Vector(n))
                }
                Some(fconfig::Value::Vector(fconfig::VectorValue::Int32Vector(n))) => {
                    ConfigValue::Vector(ConfigVectorValue::Int32Vector(n))
                }
                Some(fconfig::Value::Vector(fconfig::VectorValue::Int64Vector(n))) => {
                    ConfigValue::Vector(ConfigVectorValue::Int64Vector(n))
                }
                Some(fconfig::Value::Vector(fconfig::VectorValue::StringVector(n))) => {
                    ConfigValue::Vector(ConfigVectorValue::StringVector(n))
                }
                Some(other) => panic!("expected known variant, got {other:?}"),
                None => panic!("expected Some, got None, validation must have not been called"),
            },
        }
    }
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[fidl_decl(fidl_union = "fdecl::ConfigValue")]
pub enum ConfigValue {
    Single(ConfigSingleValue),
    Vector(ConfigVectorValue),
}

impl ConfigValue {
    /// Return the type of this value.
    pub fn ty(&self) -> ConfigValueType {
        match self {
            Self::Single(sv) => sv.ty(),
            Self::Vector(vv) => vv.ty(),
        }
    }
}

impl fmt::Display for ConfigValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigValue::Single(sv) => sv.fmt(f),
            ConfigValue::Vector(lv) => lv.fmt(f),
        }
    }
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[fidl_decl(fidl_union = "fdecl::ConfigSingleValue")]
pub enum ConfigSingleValue {
    Bool(bool),
    Uint8(u8),
    Uint16(u16),
    Uint32(u32),
    Uint64(u64),
    Int8(i8),
    Int16(i16),
    Int32(i32),
    Int64(i64),
    String(String),
}

impl ConfigSingleValue {
    fn ty(&self) -> ConfigValueType {
        match self {
            ConfigSingleValue::Bool(_) => ConfigValueType::Bool,
            ConfigSingleValue::Uint8(_) => ConfigValueType::Uint8,
            ConfigSingleValue::Uint16(_) => ConfigValueType::Uint16,
            ConfigSingleValue::Uint32(_) => ConfigValueType::Uint32,
            ConfigSingleValue::Uint64(_) => ConfigValueType::Uint64,
            ConfigSingleValue::Int8(_) => ConfigValueType::Int8,
            ConfigSingleValue::Int16(_) => ConfigValueType::Int16,
            ConfigSingleValue::Int32(_) => ConfigValueType::Int32,
            ConfigSingleValue::Int64(_) => ConfigValueType::Int64,
            // We substitute the max size limit because the value itself doesn't carry the info.
            ConfigSingleValue::String(_) => ConfigValueType::String { max_size: std::u32::MAX },
        }
    }
}

impl fmt::Display for ConfigSingleValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ConfigSingleValue::*;
        match self {
            Bool(v) => write!(f, "{}", v),
            Uint8(v) => write!(f, "{}", v),
            Uint16(v) => write!(f, "{}", v),
            Uint32(v) => write!(f, "{}", v),
            Uint64(v) => write!(f, "{}", v),
            Int8(v) => write!(f, "{}", v),
            Int16(v) => write!(f, "{}", v),
            Int32(v) => write!(f, "{}", v),
            Int64(v) => write!(f, "{}", v),
            String(v) => write!(f, "\"{}\"", v),
        }
    }
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[fidl_decl(fidl_union = "fdecl::ConfigVectorValue")]
pub enum ConfigVectorValue {
    BoolVector(Vec<bool>),
    Uint8Vector(Vec<u8>),
    Uint16Vector(Vec<u16>),
    Uint32Vector(Vec<u32>),
    Uint64Vector(Vec<u64>),
    Int8Vector(Vec<i8>),
    Int16Vector(Vec<i16>),
    Int32Vector(Vec<i32>),
    Int64Vector(Vec<i64>),
    StringVector(Vec<String>),
}

impl ConfigVectorValue {
    fn ty(&self) -> ConfigValueType {
        // We substitute the max size limit because the value itself doesn't carry the info.
        match self {
            ConfigVectorValue::BoolVector(_) => ConfigValueType::Vector {
                nested_type: ConfigNestedValueType::Bool,
                max_count: std::u32::MAX,
            },
            ConfigVectorValue::Uint8Vector(_) => ConfigValueType::Vector {
                nested_type: ConfigNestedValueType::Uint8,
                max_count: std::u32::MAX,
            },
            ConfigVectorValue::Uint16Vector(_) => ConfigValueType::Vector {
                nested_type: ConfigNestedValueType::Uint16,
                max_count: std::u32::MAX,
            },
            ConfigVectorValue::Uint32Vector(_) => ConfigValueType::Vector {
                nested_type: ConfigNestedValueType::Uint32,
                max_count: std::u32::MAX,
            },
            ConfigVectorValue::Uint64Vector(_) => ConfigValueType::Vector {
                nested_type: ConfigNestedValueType::Uint64,
                max_count: std::u32::MAX,
            },
            ConfigVectorValue::Int8Vector(_) => ConfigValueType::Vector {
                nested_type: ConfigNestedValueType::Int8,
                max_count: std::u32::MAX,
            },
            ConfigVectorValue::Int16Vector(_) => ConfigValueType::Vector {
                nested_type: ConfigNestedValueType::Int16,
                max_count: std::u32::MAX,
            },
            ConfigVectorValue::Int32Vector(_) => ConfigValueType::Vector {
                nested_type: ConfigNestedValueType::Int32,
                max_count: std::u32::MAX,
            },
            ConfigVectorValue::Int64Vector(_) => ConfigValueType::Vector {
                nested_type: ConfigNestedValueType::Int64,
                max_count: std::u32::MAX,
            },
            ConfigVectorValue::StringVector(_) => ConfigValueType::Vector {
                nested_type: ConfigNestedValueType::String { max_size: std::u32::MAX },
                max_count: std::u32::MAX,
            },
        }
    }
}

impl fmt::Display for ConfigVectorValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ConfigVectorValue::*;
        macro_rules! print_list {
            ($f:ident, $list:ident) => {{
                $f.write_str("[")?;

                for (i, item) in $list.iter().enumerate() {
                    if i > 0 {
                        $f.write_str(", ")?;
                    }
                    write!($f, "{}", item)?;
                }

                $f.write_str("]")
            }};
        }
        match self {
            BoolVector(l) => print_list!(f, l),
            Uint8Vector(l) => print_list!(f, l),
            Uint16Vector(l) => print_list!(f, l),
            Uint32Vector(l) => print_list!(f, l),
            Uint64Vector(l) => print_list!(f, l),
            Int8Vector(l) => print_list!(f, l),
            Int16Vector(l) => print_list!(f, l),
            Int32Vector(l) => print_list!(f, l),
            Int64Vector(l) => print_list!(f, l),
            StringVector(l) => {
                f.write_str("[")?;
                for (i, item) in l.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "\"{}\"", item)?;
                }
                f.write_str("]")
            }
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::RunnerRegistration")]
pub struct RunnerRegistration {
    pub source_name: CapabilityName,
    pub target_name: CapabilityName,
    pub source: RegistrationSource,
}

impl SourceName for RunnerRegistration {
    fn source_name(&self) -> &CapabilityName {
        &self.source_name
    }
}

impl RegistrationDeclCommon for RunnerRegistration {
    const TYPE: &'static str = "runner";

    fn source(&self) -> &RegistrationSource {
        &self.source
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::ResolverRegistration")]
pub struct ResolverRegistration {
    pub resolver: CapabilityName,
    pub source: RegistrationSource,
    pub scheme: String,
}

impl SourceName for ResolverRegistration {
    fn source_name(&self) -> &CapabilityName {
        &self.resolver
    }
}

impl RegistrationDeclCommon for ResolverRegistration {
    const TYPE: &'static str = "resolver";

    fn source(&self) -> &RegistrationSource {
        &self.source
    }
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_union = "fdecl::DebugRegistration")]
pub enum DebugRegistration {
    Protocol(DebugProtocolRegistration),
}

impl RegistrationDeclCommon for DebugRegistration {
    const TYPE: &'static str = "debug_protocol";

    fn source(&self) -> &RegistrationSource {
        match self {
            DebugRegistration::Protocol(protocol_reg) => &protocol_reg.source,
        }
    }
}

impl SourceName for DebugRegistration {
    fn source_name(&self) -> &CapabilityName {
        match self {
            DebugRegistration::Protocol(protocol_reg) => &protocol_reg.source_name,
        }
    }
}

#[derive(FidlDecl, Debug, Clone, PartialEq, Eq)]
#[fidl_decl(fidl_table = "fdecl::DebugProtocolRegistration")]
pub struct DebugProtocolRegistration {
    pub source_name: CapabilityName,
    pub source: RegistrationSource,
    pub target_name: CapabilityName,
}

#[derive(FidlDecl, Debug, Clone, PartialEq)]
#[fidl_decl(fidl_table = "fdecl::Program")]
pub struct ProgramDecl {
    pub runner: Option<CapabilityName>,
    pub info: fdata::Dictionary,
}

impl Default for ProgramDecl {
    fn default() -> Self {
        Self { runner: None, info: fdata::Dictionary::default().clone() }
    }
}

fidl_translations_identical!([u8; 32]);
fidl_translations_identical!(u8);
fidl_translations_identical!(u16);
fidl_translations_identical!(u32);
fidl_translations_identical!(u64);
fidl_translations_identical!(i8);
fidl_translations_identical!(i16);
fidl_translations_identical!(i32);
fidl_translations_identical!(i64);
fidl_translations_identical!(bool);
fidl_translations_identical!(String);
fidl_translations_identical!(Vec<CapabilityName>);
fidl_translations_identical!(fdecl::StartupMode);
fidl_translations_identical!(fdecl::OnTerminate);
fidl_translations_identical!(fdecl::Durability);
fidl_translations_identical!(fdata::Dictionary);
fidl_translations_identical!(fio::Operations);
fidl_translations_identical!(fdecl::EnvironmentExtends);
fidl_translations_identical!(fdecl::StorageId);
fidl_translations_identical!(Vec<fprocess::HandleInfo>);

fidl_translations_from_into!(cm_types::AllowedOffers, fdecl::AllowedOffers);
fidl_translations_from_into!(CapabilityName, String);

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyType {
    Strong,
    Weak,
    WeakForMigration,
}

fidl_translations_symmetrical_enums!(
    fdecl::DependencyType,
    DependencyType,
    Strong,
    Weak,
    WeakForMigration
);

/// A path to a capability.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CapabilityPath {
    /// The directory containing the last path element, e.g. `/svc/foo` in `/svc/foo/bar`.
    pub dirname: FlyStr,
    /// The last path element: e.g. `bar` in `/svc/foo/bar`.
    pub basename: FlyStr,
}

impl CapabilityPath {
    pub fn to_path_buf(&self) -> PathBuf {
        PathBuf::from(self.to_string())
    }

    /// Splits the path according to "/", ignoring empty path components
    pub fn split(&self) -> Vec<String> {
        self.to_string().split("/").map(|s| s.to_string()).filter(|s| !s.is_empty()).collect()
    }
}

impl FromStr for CapabilityPath {
    type Err = Error;

    fn from_str(path: &str) -> Result<CapabilityPath, Error> {
        cm_types::Path::validate(path)
            .map_err(|_| Error::InvalidCapabilityPath { raw: path.to_string() })?;
        let idx = path.rfind('/').expect("path validation is wrong");
        Ok(CapabilityPath {
            dirname: if idx == 0 { "/".into() } else { path[0..idx].into() },
            basename: path[idx + 1..].into(),
        })
    }
}

impl TryFrom<&str> for CapabilityPath {
    type Error = Error;

    fn try_from(path: &str) -> Result<CapabilityPath, Error> {
        Self::from_str(path)
    }
}

impl fmt::Display for CapabilityPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if &self.dirname == "/" {
            write!(f, "/{}", self.basename)
        } else {
            write!(f, "{}/{}", self.dirname, self.basename)
        }
    }
}

impl UseDecl {
    pub fn path(&self) -> Option<&CapabilityPath> {
        match self {
            UseDecl::Service(d) => Some(&d.target_path),
            UseDecl::Protocol(d) => Some(&d.target_path),
            UseDecl::Directory(d) => Some(&d.target_path),
            UseDecl::Storage(d) => Some(&d.target_path),
            UseDecl::EventStream(d) => Some(&d.target_path),
        }
    }

    pub fn name(&self) -> Option<&CapabilityName> {
        match self {
            UseDecl::Storage(storage_decl) => Some(&storage_decl.source_name),
            UseDecl::EventStream(_) => None,
            UseDecl::Service(_) | UseDecl::Protocol(_) | UseDecl::Directory(_) => None,
        }
    }
}

impl SourceName for UseDecl {
    fn source_name(&self) -> &CapabilityName {
        match self {
            UseDecl::Storage(storage_decl) => &storage_decl.source_name,
            UseDecl::Service(service_decl) => &service_decl.source_name,
            UseDecl::Protocol(protocol_decl) => &protocol_decl.source_name,
            UseDecl::Directory(directory_decl) => &directory_decl.source_name,
            UseDecl::EventStream(event_stream_decl) => &event_stream_decl.source_name,
        }
    }
}

/// A named capability.
///
/// Unlike a `CapabilityPath`, a `CapabilityName` doesn't encode any form
/// of hierarchy.
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct CapabilityName(FlyStr);

impl CapabilityName {
    pub fn str(&self) -> &str {
        &*self.0
    }
}

impl From<CapabilityName> for String {
    fn from(name: CapabilityName) -> String {
        name.0.into()
    }
}

impl From<&str> for CapabilityName {
    fn from(name: &str) -> CapabilityName {
        CapabilityName(FlyStr::new(name))
    }
}

impl From<String> for CapabilityName {
    fn from(name: String) -> CapabilityName {
        CapabilityName(name.into())
    }
}

impl From<&String> for CapabilityName {
    fn from(name: &String) -> CapabilityName {
        CapabilityName(name.into())
    }
}

impl FromStr for CapabilityName {
    type Err = Error;

    fn from_str(name: &str) -> Result<CapabilityName, Error> {
        Ok(name.into())
    }
}

impl From<cm_types::Name> for CapabilityName {
    fn from(name: cm_types::Name) -> CapabilityName {
        name.as_str().into()
    }
}

impl PartialEq<str> for CapabilityName {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl<'a> PartialEq<&'a str> for CapabilityName {
    fn eq(&self, other: &&'a str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for CapabilityName {
    fn eq(&self, other: &String) -> bool {
        self.0 == other.as_str()
    }
}

impl fmt::Display for CapabilityName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// The trait for all declarations that have a source name.
pub trait SourceName {
    fn source_name(&self) -> &CapabilityName;
}

/// The common properties of a [Use](fdecl::Use) declaration.
pub trait UseDeclCommon: SourceName + Send + Sync {
    fn source(&self) -> &UseSource;
    fn availability(&self) -> &Availability;
}

/// The common properties of a Registration-with-environment declaration.
pub trait RegistrationDeclCommon: SourceName + Send + Sync {
    /// The name of the registration type, for error messages.
    const TYPE: &'static str;
    fn source(&self) -> &RegistrationSource;
}

/// The common properties of an [Offer](fdecl::Offer) declaration.
pub trait OfferDeclCommon: SourceName + fmt::Debug + Send + Sync {
    fn target_name(&self) -> &CapabilityName;
    fn target(&self) -> &OfferTarget;
    fn source(&self) -> &OfferSource;
    fn availability(&self) -> Option<&Availability>;
}

/// The common properties of an [Expose](fdecl::Expose) declaration.
pub trait ExposeDeclCommon: SourceName + fmt::Debug + Send + Sync {
    fn target_name(&self) -> &CapabilityName;
    fn target(&self) -> &ExposeTarget;
    fn source(&self) -> &ExposeSource;
    fn availability(&self) -> &Availability;
}

/// The common properties of a [Capability](fdecl::Capability) declaration.
pub trait CapabilityDeclCommon: Send + Sync {
    fn name(&self) -> &CapabilityName;
}

/// A named capability type.
///
/// `CapabilityTypeName` provides a user friendly type encoding for a capability.
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CapabilityTypeName {
    Directory,
    EventStream,
    Protocol,
    Resolver,
    Runner,
    Service,
    Storage,
}

impl fmt::Display for CapabilityTypeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let display_name = match &self {
            CapabilityTypeName::Directory => "directory",
            CapabilityTypeName::EventStream => "event_stream",
            CapabilityTypeName::Protocol => "protocol",
            CapabilityTypeName::Resolver => "resolver",
            CapabilityTypeName::Runner => "runner",
            CapabilityTypeName::Service => "service",
            CapabilityTypeName::Storage => "storage",
        };
        write!(f, "{}", display_name)
    }
}

impl From<&UseDecl> for CapabilityTypeName {
    fn from(use_decl: &UseDecl) -> Self {
        match use_decl {
            UseDecl::Service(_) => Self::Service,
            UseDecl::Protocol(_) => Self::Protocol,
            UseDecl::Directory(_) => Self::Directory,
            UseDecl::Storage(_) => Self::Storage,
            UseDecl::EventStream(_) => Self::EventStream,
        }
    }
}

impl From<&OfferDecl> for CapabilityTypeName {
    fn from(offer_decl: &OfferDecl) -> Self {
        match offer_decl {
            OfferDecl::Service(_) => Self::Service,
            OfferDecl::Protocol(_) => Self::Protocol,
            OfferDecl::Directory(_) => Self::Directory,
            OfferDecl::Storage(_) => Self::Storage,
            OfferDecl::Runner(_) => Self::Runner,
            OfferDecl::Resolver(_) => Self::Resolver,
            OfferDecl::EventStream(_) => Self::EventStream,
        }
    }
}

impl From<&ExposeDecl> for CapabilityTypeName {
    fn from(expose_decl: &ExposeDecl) -> Self {
        match expose_decl {
            ExposeDecl::Service(_) => Self::Service,
            ExposeDecl::Protocol(_) => Self::Protocol,
            ExposeDecl::Directory(_) => Self::Directory,
            ExposeDecl::Runner(_) => Self::Runner,
            ExposeDecl::Resolver(_) => Self::Resolver,
        }
    }
}

// TODO: Runners and third parties can use this to parse `facets`.
impl FidlIntoNative<HashMap<String, DictionaryValue>> for fdata::Dictionary {
    fn fidl_into_native(self) -> HashMap<String, DictionaryValue> {
        from_fidl_dict(self)
    }
}

impl NativeIntoFidl<fdata::Dictionary> for HashMap<String, DictionaryValue> {
    fn native_into_fidl(self) -> fdata::Dictionary {
        to_fidl_dict(self)
    }
}

impl FidlIntoNative<BTreeMap<String, DictionaryValue>> for fdata::Dictionary {
    fn fidl_into_native(self) -> BTreeMap<String, DictionaryValue> {
        from_fidl_dict_btree(self)
    }
}

impl NativeIntoFidl<fdata::Dictionary> for BTreeMap<String, DictionaryValue> {
    fn native_into_fidl(self) -> fdata::Dictionary {
        to_fidl_dict_btree(self)
    }
}

impl FidlIntoNative<CapabilityPath> for String {
    fn fidl_into_native(self) -> CapabilityPath {
        self.as_str().parse().unwrap()
    }
}

impl NativeIntoFidl<String> for CapabilityPath {
    fn native_into_fidl(self) -> String {
        self.to_string()
    }
}

impl FidlIntoNative<PathBuf> for String {
    fn fidl_into_native(self) -> PathBuf {
        PathBuf::from(self)
    }
}

impl NativeIntoFidl<String> for PathBuf {
    fn native_into_fidl(self) -> String {
        self.into_os_string().into_string().expect("invalid utf8")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DictionaryValue {
    Str(String),
    StrVec(Vec<String>),
    Null,
}

impl FidlIntoNative<DictionaryValue> for Option<Box<fdata::DictionaryValue>> {
    fn fidl_into_native(self) -> DictionaryValue {
        // Temporarily allow unreachable patterns while fuchsia.data.DictionaryValue
        // is migrated from `strict` to `flexible`.
        // TODO(https://fxbug.dev/92247): Remove this.
        #[allow(unreachable_patterns)]
        match self {
            Some(v) => match *v {
                fdata::DictionaryValue::Str(s) => DictionaryValue::Str(s),
                fdata::DictionaryValue::StrVec(ss) => DictionaryValue::StrVec(ss),
                _ => DictionaryValue::Null,
            },
            None => DictionaryValue::Null,
        }
    }
}

impl NativeIntoFidl<Option<Box<fdata::DictionaryValue>>> for DictionaryValue {
    fn native_into_fidl(self) -> Option<Box<fdata::DictionaryValue>> {
        match self {
            DictionaryValue::Str(s) => Some(Box::new(fdata::DictionaryValue::Str(s))),
            DictionaryValue::StrVec(ss) => Some(Box::new(fdata::DictionaryValue::StrVec(ss))),
            DictionaryValue::Null => None,
        }
    }
}

fn from_fidl_dict(dict: fdata::Dictionary) -> HashMap<String, DictionaryValue> {
    match dict.entries {
        Some(entries) => entries.into_iter().map(|e| (e.key, e.value.fidl_into_native())).collect(),
        _ => HashMap::new(),
    }
}

fn to_fidl_dict(dict: HashMap<String, DictionaryValue>) -> fdata::Dictionary {
    fdata::Dictionary {
        entries: Some(
            dict.into_iter()
                .map(|(key, value)| fdata::DictionaryEntry { key, value: value.native_into_fidl() })
                .collect(),
        ),
        ..Default::default()
    }
}

fn from_fidl_dict_btree(dict: fdata::Dictionary) -> BTreeMap<String, DictionaryValue> {
    match dict.entries {
        Some(entries) => entries.into_iter().map(|e| (e.key, e.value.fidl_into_native())).collect(),
        _ => BTreeMap::new(),
    }
}

fn to_fidl_dict_btree(dict: BTreeMap<String, DictionaryValue>) -> fdata::Dictionary {
    fdata::Dictionary {
        entries: Some(
            dict.into_iter()
                .map(|(key, value)| fdata::DictionaryEntry { key, value: value.native_into_fidl() })
                .collect(),
        ),
        ..Default::default()
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UseSource {
    Parent,
    Framework,
    Debug,
    Self_,
    Capability(CapabilityName),
    Child(String),
}

impl std::fmt::Display for UseSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Framework => write!(f, "framework"),
            Self::Parent => write!(f, "parent"),
            Self::Debug => write!(f, "debug environment"),
            Self::Self_ => write!(f, "self"),
            Self::Capability(c) => write!(f, "capability `{}`", c),
            Self::Child(c) => write!(f, "child `#{}`", c),
        }
    }
}

impl FidlIntoNative<UseSource> for fdecl::Ref {
    fn fidl_into_native(self) -> UseSource {
        match self {
            fdecl::Ref::Parent(_) => UseSource::Parent,
            fdecl::Ref::Framework(_) => UseSource::Framework,
            fdecl::Ref::Debug(_) => UseSource::Debug,
            fdecl::Ref::Self_(_) => UseSource::Self_,
            fdecl::Ref::Capability(c) => UseSource::Capability(c.name.into()),
            fdecl::Ref::Child(c) => UseSource::Child(c.name),
            _ => panic!("invalid UseSource variant"),
        }
    }
}

impl NativeIntoFidl<fdecl::Ref> for UseSource {
    fn native_into_fidl(self) -> fdecl::Ref {
        match self {
            UseSource::Parent => fdecl::Ref::Parent(fdecl::ParentRef {}),
            UseSource::Framework => fdecl::Ref::Framework(fdecl::FrameworkRef {}),
            UseSource::Debug => fdecl::Ref::Debug(fdecl::DebugRef {}),
            UseSource::Self_ => fdecl::Ref::Self_(fdecl::SelfRef {}),
            UseSource::Capability(name) => {
                fdecl::Ref::Capability(fdecl::CapabilityRef { name: name.to_string() })
            }
            UseSource::Child(name) => fdecl::Ref::Child(fdecl::ChildRef { name, collection: None }),
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EventScope {
    Child(ChildRef),
    Collection(String),
}

impl FidlIntoNative<EventScope> for fdecl::Ref {
    fn fidl_into_native(self) -> EventScope {
        match self {
            fdecl::Ref::Child(c) => {
                if let Some(_) = c.collection {
                    panic!("Dynamic children scopes are not supported for EventStreams");
                } else {
                    EventScope::Child(ChildRef { name: c.name.into(), collection: None })
                }
            }
            fdecl::Ref::Collection(collection) => EventScope::Collection(collection.name),
            _ => panic!("invalid EventScope variant"),
        }
    }
}

impl NativeIntoFidl<fdecl::Ref> for EventScope {
    fn native_into_fidl(self) -> fdecl::Ref {
        match self {
            EventScope::Child(child) => fdecl::Ref::Child(child.native_into_fidl()),
            EventScope::Collection(name) => fdecl::Ref::Collection(fdecl::CollectionRef { name }),
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfferSource {
    Framework,
    Parent,
    Child(ChildRef),
    Collection(String),
    Self_,
    Capability(CapabilityName),
    Void,
}

impl std::fmt::Display for OfferSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Framework => write!(f, "framework"),
            Self::Parent => write!(f, "parent"),
            Self::Child(c) => write!(f, "child `#{}`", c),
            Self::Collection(c) => write!(f, "collection `#{}`", c),
            Self::Self_ => write!(f, "self"),
            Self::Capability(c) => write!(f, "capability `{}`", c),
            Self::Void => write!(f, "void"),
        }
    }
}

impl OfferSource {
    pub fn static_child(name: String) -> Self {
        Self::Child(ChildRef { name: name.into(), collection: None })
    }
}

impl FidlIntoNative<OfferSource> for fdecl::Ref {
    fn fidl_into_native(self) -> OfferSource {
        match self {
            fdecl::Ref::Parent(_) => OfferSource::Parent,
            fdecl::Ref::Self_(_) => OfferSource::Self_,
            fdecl::Ref::Child(c) => OfferSource::Child(c.fidl_into_native()),
            fdecl::Ref::Collection(c) => OfferSource::Collection(c.name),
            fdecl::Ref::Framework(_) => OfferSource::Framework,
            fdecl::Ref::Capability(c) => OfferSource::Capability(c.name.into()),
            fdecl::Ref::VoidType(_) => OfferSource::Void,
            _ => panic!("invalid OfferSource variant"),
        }
    }
}

impl NativeIntoFidl<fdecl::Ref> for OfferSource {
    fn native_into_fidl(self) -> fdecl::Ref {
        match self {
            OfferSource::Parent => fdecl::Ref::Parent(fdecl::ParentRef {}),
            OfferSource::Self_ => fdecl::Ref::Self_(fdecl::SelfRef {}),
            OfferSource::Child(c) => fdecl::Ref::Child(c.native_into_fidl()),
            OfferSource::Collection(name) => fdecl::Ref::Collection(fdecl::CollectionRef { name }),
            OfferSource::Framework => fdecl::Ref::Framework(fdecl::FrameworkRef {}),
            OfferSource::Capability(name) => {
                fdecl::Ref::Capability(fdecl::CapabilityRef { name: name.to_string() })
            }
            OfferSource::Void => fdecl::Ref::VoidType(fdecl::VoidRef {}),
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExposeSource {
    Self_,
    Child(String),
    Collection(String),
    Framework,
    Capability(CapabilityName),
    Void,
}

impl std::fmt::Display for ExposeSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Framework => write!(f, "framework"),
            Self::Child(c) => write!(f, "child `#{}`", c),
            Self::Collection(c) => write!(f, "collection `#{}`", c),
            Self::Self_ => write!(f, "self"),
            Self::Capability(c) => write!(f, "capability `{}`", c),
            Self::Void => write!(f, "void"),
        }
    }
}

impl FidlIntoNative<ExposeSource> for fdecl::Ref {
    fn fidl_into_native(self) -> ExposeSource {
        match self {
            fdecl::Ref::Self_(_) => ExposeSource::Self_,
            fdecl::Ref::Child(c) => ExposeSource::Child(c.name),
            fdecl::Ref::Collection(c) => ExposeSource::Collection(c.name),
            fdecl::Ref::Framework(_) => ExposeSource::Framework,
            fdecl::Ref::Capability(c) => ExposeSource::Capability(c.name.into()),
            fdecl::Ref::VoidType(_) => ExposeSource::Void,
            _ => panic!("invalid ExposeSource variant"),
        }
    }
}

impl NativeIntoFidl<fdecl::Ref> for ExposeSource {
    fn native_into_fidl(self) -> fdecl::Ref {
        match self {
            ExposeSource::Self_ => fdecl::Ref::Self_(fdecl::SelfRef {}),
            ExposeSource::Child(child_name) => {
                fdecl::Ref::Child(fdecl::ChildRef { name: child_name, collection: None })
            }
            ExposeSource::Collection(name) => fdecl::Ref::Collection(fdecl::CollectionRef { name }),
            ExposeSource::Framework => fdecl::Ref::Framework(fdecl::FrameworkRef {}),
            ExposeSource::Capability(name) => {
                fdecl::Ref::Capability(fdecl::CapabilityRef { name: name.to_string() })
            }
            ExposeSource::Void => fdecl::Ref::VoidType(fdecl::VoidRef {}),
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ExposeTarget {
    Parent,
    Framework,
}

impl std::fmt::Display for ExposeTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Framework => write!(f, "framework"),
            Self::Parent => write!(f, "parent"),
        }
    }
}

impl FidlIntoNative<ExposeTarget> for fdecl::Ref {
    fn fidl_into_native(self) -> ExposeTarget {
        match self {
            fdecl::Ref::Parent(_) => ExposeTarget::Parent,
            fdecl::Ref::Framework(_) => ExposeTarget::Framework,
            _ => panic!("invalid ExposeTarget variant"),
        }
    }
}

impl NativeIntoFidl<fdecl::Ref> for ExposeTarget {
    fn native_into_fidl(self) -> fdecl::Ref {
        match self {
            ExposeTarget::Parent => fdecl::Ref::Parent(fdecl::ParentRef {}),
            ExposeTarget::Framework => fdecl::Ref::Framework(fdecl::FrameworkRef {}),
        }
    }
}

/// A source for a service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceSource<T> {
    /// The provider of the service, relative to a component.
    pub source: T,
    /// The name of the service.
    pub source_name: CapabilityName,
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageDirectorySource {
    Parent,
    Self_,
    Child(String),
}

impl FidlIntoNative<StorageDirectorySource> for fdecl::Ref {
    fn fidl_into_native(self) -> StorageDirectorySource {
        match self {
            fdecl::Ref::Parent(_) => StorageDirectorySource::Parent,
            fdecl::Ref::Self_(_) => StorageDirectorySource::Self_,
            fdecl::Ref::Child(c) => StorageDirectorySource::Child(c.name),
            _ => panic!("invalid OfferDirectorySource variant"),
        }
    }
}

impl NativeIntoFidl<fdecl::Ref> for StorageDirectorySource {
    fn native_into_fidl(self) -> fdecl::Ref {
        match self {
            StorageDirectorySource::Parent => fdecl::Ref::Parent(fdecl::ParentRef {}),
            StorageDirectorySource::Self_ => fdecl::Ref::Self_(fdecl::SelfRef {}),
            StorageDirectorySource::Child(child_name) => {
                fdecl::Ref::Child(fdecl::ChildRef { name: child_name, collection: None })
            }
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationSource {
    Parent,
    Self_,
    Child(String),
}

impl FidlIntoNative<RegistrationSource> for fdecl::Ref {
    fn fidl_into_native(self) -> RegistrationSource {
        match self {
            fdecl::Ref::Parent(_) => RegistrationSource::Parent,
            fdecl::Ref::Self_(_) => RegistrationSource::Self_,
            fdecl::Ref::Child(c) => RegistrationSource::Child(c.name),
            _ => panic!("invalid RegistrationSource variant"),
        }
    }
}

impl NativeIntoFidl<fdecl::Ref> for RegistrationSource {
    fn native_into_fidl(self) -> fdecl::Ref {
        match self {
            RegistrationSource::Parent => fdecl::Ref::Parent(fdecl::ParentRef {}),
            RegistrationSource::Self_ => fdecl::Ref::Self_(fdecl::SelfRef {}),
            RegistrationSource::Child(child_name) => {
                fdecl::Ref::Child(fdecl::ChildRef { name: child_name, collection: None })
            }
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OfferTarget {
    Child(ChildRef),
    Collection(String),
}
impl OfferTarget {
    pub fn static_child(name: String) -> Self {
        Self::Child(ChildRef { name: name.into(), collection: None })
    }
}

impl std::fmt::Display for OfferTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Child(c) => write!(f, "child `#{}`", c),
            Self::Collection(c) => write!(f, "collection `#{}`", c),
        }
    }
}

impl FidlIntoNative<OfferTarget> for fdecl::Ref {
    fn fidl_into_native(self) -> OfferTarget {
        match self {
            fdecl::Ref::Child(c) => OfferTarget::Child(c.fidl_into_native()),
            fdecl::Ref::Collection(c) => OfferTarget::Collection(c.name),
            _ => panic!("invalid OfferTarget variant"),
        }
    }
}

impl NativeIntoFidl<fdecl::Ref> for OfferTarget {
    fn native_into_fidl(self) -> fdecl::Ref {
        match self {
            OfferTarget::Child(c) => fdecl::Ref::Child(c.native_into_fidl()),
            OfferTarget::Collection(collection_name) => {
                fdecl::Ref::Collection(fdecl::CollectionRef { name: collection_name })
            }
        }
    }
}

/// Converts the contents of a CM-FIDL declaration and produces the equivalent CM-Rust
/// struct.
/// This function applies cm_fidl_validator to check correctness.
impl TryFrom<fdecl::Component> for ComponentDecl {
    type Error = Error;

    fn try_from(decl: fdecl::Component) -> Result<Self, Self::Error> {
        cm_fidl_validator::validate(&decl).map_err(|err| Error::Validate { err })?;
        Ok(decl.fidl_into_native())
    }
}

// Converts the contents of a CM-Rust declaration into a CM_FIDL declaration
impl From<ComponentDecl> for fdecl::Component {
    fn from(decl: ComponentDecl) -> Self {
        decl.native_into_fidl()
    }
}

/// Errors produced by cm_rust.
#[derive(Debug, Error, Clone)]
pub enum Error {
    #[error("Fidl validation failed: {}", err)]
    Validate {
        #[source]
        err: cm_fidl_validator::error::ErrorList,
    },
    #[error("Invalid capability path: {}", raw)]
    InvalidCapabilityPath { raw: String },
}

#[cfg(test)]
mod tests {
    use {super::*, fidl_fuchsia_component_decl as fdecl, std::convert::TryInto};

    macro_rules! test_try_from_decl {
        (
            $(
                $test_name:ident => {
                    input = $input:expr,
                    result = $result:expr,
                },
            )+
        ) => {
            $(
                #[test]
                fn $test_name() {
                    {
                        let res = ComponentDecl::try_from($input).expect("try_from failed");
                        assert_eq!(res, $result, "Conversion from FIDL type into cm_rust ComponentDecl type did not match expected result (left is expected, right is actual)");
                    }
                    {
                        let res = fdecl::Component::try_from($result).expect("try_from failed");
                        assert_eq!($input, res, "Conversion from cm_rust ComponentDecl type into FIDL type did not match expected result (left is expected, right is actual)");
                    }
                }
            )+
        }
    }

    macro_rules! test_fidl_into_and_from {
        (
            $(
                $test_name:ident => {
                    input = $input:expr,
                    input_type = $input_type:ty,
                    result = $result:expr,
                    result_type = $result_type:ty,
                },
            )+
        ) => {
            $(
                #[test]
                fn $test_name() {
                    {
                        let res: Vec<$result_type> =
                            $input.into_iter().map(|e| e.fidl_into_native()).collect();
                        assert_eq!(res, $result);
                    }
                    {
                        let res: Vec<$input_type> =
                            $result.into_iter().map(|e| e.native_into_fidl()).collect();
                        assert_eq!(res, $input);
                    }
                }
            )+
        }
    }

    macro_rules! test_fidl_into {
        (
            $(
                $test_name:ident => {
                    input = $input:expr,
                    result = $result:expr,
                },
            )+
        ) => {
            $(
                #[test]
                fn $test_name() {
                    test_fidl_into_helper($input, $result);
                }
            )+
        }
    }

    fn test_fidl_into_helper<T, U>(input: T, expected_res: U)
    where
        T: FidlIntoNative<U>,
        U: std::cmp::PartialEq + std::fmt::Debug,
    {
        let res: U = input.fidl_into_native();
        assert_eq!(res, expected_res);
    }

    macro_rules! test_capability_path {
        (
            $(
                $test_name:ident => {
                    input = $input:expr,
                    result = $result:expr,
                },
            )+
        ) => {
            $(
                #[test]
                fn $test_name() {
                    test_capability_path_helper($input, $result);
                }
            )+
        }
    }

    fn test_capability_path_helper(input: &str, result: Result<CapabilityPath, Error>) {
        let res = CapabilityPath::try_from(input);
        assert_eq!(format!("{:?}", res), format!("{:?}", result));
        if let Ok(p) = res {
            assert_eq!(&p.to_string(), input);
        }
    }

    test_try_from_decl! {
        try_from_empty => {
            input = fdecl::Component {
                program: None,
                uses: None,
                exposes: None,
                offers: None,
                capabilities: None,
                children: None,
                collections: None,
                facets: None,
                environments: None,
                ..Default::default()
            },
            result = ComponentDecl {
                program: None,
                uses: vec![],
                exposes: vec![],
                offers: vec![],
                capabilities: vec![],
                children: vec![],
                collections: vec![],
                facets: None,
                environments: vec![],
                config: None,
            },
        },
        try_from_all => {
            input = fdecl::Component {
                program: Some(fdecl::Program {
                    runner: Some("elf".to_string()),
                    info: Some(fdata::Dictionary {
                        entries: Some(vec![
                            fdata::DictionaryEntry {
                                key: "args".to_string(),
                                value: Some(Box::new(fdata::DictionaryValue::StrVec(vec!["foo".to_string(), "bar".to_string()]))),
                            },
                            fdata::DictionaryEntry {
                                key: "binary".to_string(),
                                value: Some(Box::new(fdata::DictionaryValue::Str("bin/app".to_string()))),
                            },
                        ]),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                uses: Some(vec![
                    fdecl::Use::Service(fdecl::UseService {
                        dependency_type: Some(fdecl::DependencyType::Strong),
                        source: Some(fdecl::Ref::Parent(fdecl::ParentRef {})),
                        source_name: Some("netstack".to_string()),
                        target_path: Some("/svc/mynetstack".to_string()),
                        availability: Some(fdecl::Availability::Required),
                        ..Default::default()
                    }),
                    fdecl::Use::Protocol(fdecl::UseProtocol {
                        dependency_type: Some(fdecl::DependencyType::Strong),
                        source: Some(fdecl::Ref::Parent(fdecl::ParentRef {})),
                        source_name: Some("legacy_netstack".to_string()),
                        target_path: Some("/svc/legacy_mynetstack".to_string()),
                        availability: Some(fdecl::Availability::Optional),
                        ..Default::default()
                    }),
                    fdecl::Use::Protocol(fdecl::UseProtocol {
                        dependency_type: Some(fdecl::DependencyType::Strong),
                        source: Some(fdecl::Ref::Child(fdecl::ChildRef { name: "echo".to_string(), collection: None})),
                        source_name: Some("echo_service".to_string()),
                        target_path: Some("/svc/echo_service".to_string()),
                        availability: Some(fdecl::Availability::Required),
                        ..Default::default()
                    }),
                    fdecl::Use::Directory(fdecl::UseDirectory {
                        dependency_type: Some(fdecl::DependencyType::Strong),
                        source: Some(fdecl::Ref::Framework(fdecl::FrameworkRef {})),
                        source_name: Some("dir".to_string()),
                        target_path: Some("/data".to_string()),
                        rights: Some(fio::Operations::CONNECT),
                        subdir: Some("foo/bar".to_string()),
                        availability: Some(fdecl::Availability::Required),
                        ..Default::default()
                    }),
                    fdecl::Use::Storage(fdecl::UseStorage {
                        source_name: Some("cache".to_string()),
                        target_path: Some("/cache".to_string()),
                        availability: Some(fdecl::Availability::Required),
                        ..Default::default()
                    }),
                    fdecl::Use::Storage(fdecl::UseStorage {
                        source_name: Some("temp".to_string()),
                        target_path: Some("/temp".to_string()),
                        availability: Some(fdecl::Availability::Optional),
                        ..Default::default()
                    }),
                    fdecl::Use::EventStream(fdecl::UseEventStream {
                        source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                            collection: None,
                            name: "test".to_string(),
                        })),
                        source_name: Some("stopped".to_string()),
                        scope: Some(vec![
                            fdecl::Ref::Child(fdecl::ChildRef {
                                collection: None,
                                name:"a".to_string(),
                        }), fdecl::Ref::Collection(fdecl::CollectionRef {
                            name:"b".to_string(),
                        })]),
                        target_path: Some("/svc/test".to_string()),
                        availability: Some(fdecl::Availability::Optional),
                        ..Default::default()
                    }),
                ]),
                exposes: Some(vec![
                    fdecl::Expose::Protocol(fdecl::ExposeProtocol {
                        source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                            name: "netstack".to_string(),
                            collection: None,
                        })),
                        source_name: Some("legacy_netstack".to_string()),
                        target_name: Some("legacy_mynetstack".to_string()),
                        target: Some(fdecl::Ref::Parent(fdecl::ParentRef {})),
                        availability: Some(fdecl::Availability::Required),
                        ..Default::default()
                    }),
                    fdecl::Expose::Directory(fdecl::ExposeDirectory {
                        source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                            name: "netstack".to_string(),
                            collection: None,
                        })),
                        source_name: Some("dir".to_string()),
                        target_name: Some("data".to_string()),
                        target: Some(fdecl::Ref::Parent(fdecl::ParentRef {})),
                        rights: Some(fio::Operations::CONNECT),
                        subdir: Some("foo/bar".to_string()),
                        availability: Some(fdecl::Availability::Optional),
                        ..Default::default()
                    }),
                    fdecl::Expose::Runner(fdecl::ExposeRunner {
                        source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                            name: "netstack".to_string(),
                            collection: None,
                        })),
                        source_name: Some("elf".to_string()),
                        target: Some(fdecl::Ref::Parent(fdecl::ParentRef {})),
                        target_name: Some("elf".to_string()),
                        ..Default::default()
                    }),
                    fdecl::Expose::Resolver(fdecl::ExposeResolver{
                        source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                            name: "netstack".to_string(),
                            collection: None,
                        })),
                        source_name: Some("pkg".to_string()),
                        target: Some(fdecl::Ref::Parent(fdecl::ParentRef{})),
                        target_name: Some("pkg".to_string()),
                        ..Default::default()
                    }),
                    fdecl::Expose::Service(fdecl::ExposeService {
                        source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                            name: "netstack".to_string(),
                            collection: None,
                        })),
                        source_name: Some("netstack1".to_string()),
                        target_name: Some("mynetstack".to_string()),
                        target: Some(fdecl::Ref::Parent(fdecl::ParentRef {})),
                        availability: Some(fdecl::Availability::Required),
                        ..Default::default()
                    }),
                    fdecl::Expose::Service(fdecl::ExposeService {
                        source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                            name: "netstack".to_string(),
                            collection: None,
                        })),
                        source_name: Some("netstack2".to_string()),
                        target_name: Some("mynetstack".to_string()),
                        target: Some(fdecl::Ref::Parent(fdecl::ParentRef {})),
                        availability: Some(fdecl::Availability::Required),
                        ..Default::default()
                    }),
                ]),
                offers: Some(vec![
                    fdecl::Offer::Protocol(fdecl::OfferProtocol {
                        source: Some(fdecl::Ref::Parent(fdecl::ParentRef {})),
                        source_name: Some("legacy_netstack".to_string()),
                        target: Some(fdecl::Ref::Child(
                           fdecl::ChildRef {
                               name: "echo".to_string(),
                               collection: None,
                           }
                        )),
                        target_name: Some("legacy_mynetstack".to_string()),
                        dependency_type: Some(fdecl::DependencyType::WeakForMigration),
                        availability: Some(fdecl::Availability::Required),
                        ..Default::default()
                    }),
                    fdecl::Offer::Directory(fdecl::OfferDirectory {
                        source: Some(fdecl::Ref::Parent(fdecl::ParentRef {})),
                        source_name: Some("dir".to_string()),
                        target: Some(fdecl::Ref::Collection(
                            fdecl::CollectionRef { name: "modular".to_string() }
                        )),
                        target_name: Some("data".to_string()),
                        rights: Some(fio::Operations::CONNECT),
                        subdir: None,
                        dependency_type: Some(fdecl::DependencyType::Strong),
                        availability: Some(fdecl::Availability::Optional),
                        ..Default::default()
                    }),
                    fdecl::Offer::Storage(fdecl::OfferStorage {
                        source_name: Some("cache".to_string()),
                        source: Some(fdecl::Ref::Self_(fdecl::SelfRef {})),
                        target: Some(fdecl::Ref::Collection(
                            fdecl::CollectionRef { name: "modular".to_string() }
                        )),
                        target_name: Some("cache".to_string()),
                        availability: Some(fdecl::Availability::Required),
                        ..Default::default()
                    }),
                    fdecl::Offer::Runner(fdecl::OfferRunner {
                        source: Some(fdecl::Ref::Parent(fdecl::ParentRef {})),
                        source_name: Some("elf".to_string()),
                        target: Some(fdecl::Ref::Child(
                           fdecl::ChildRef {
                               name: "echo".to_string(),
                               collection: None,
                           }
                        )),
                        target_name: Some("elf2".to_string()),
                        ..Default::default()
                    }),
                    fdecl::Offer::Resolver(fdecl::OfferResolver{
                        source: Some(fdecl::Ref::Parent(fdecl::ParentRef{})),
                        source_name: Some("pkg".to_string()),
                        target: Some(fdecl::Ref::Child(
                           fdecl::ChildRef {
                              name: "echo".to_string(),
                              collection: None,
                           }
                        )),
                        target_name: Some("pkg".to_string()),
                        ..Default::default()
                    }),
                    fdecl::Offer::Service(fdecl::OfferService {
                        source: Some(fdecl::Ref::Parent(fdecl::ParentRef {})),
                        source_name: Some("netstack1".to_string()),
                        target: Some(fdecl::Ref::Child(
                           fdecl::ChildRef {
                               name: "echo".to_string(),
                               collection: None,
                           }
                        )),
                        target_name: Some("mynetstack1".to_string()),
                        availability: Some(fdecl::Availability::Required),
                        ..Default::default()
                    }),
                    fdecl::Offer::Service(fdecl::OfferService {
                        source: Some(fdecl::Ref::Parent(fdecl::ParentRef {})),
                        source_name: Some("netstack2".to_string()),
                        target: Some(fdecl::Ref::Child(
                           fdecl::ChildRef {
                               name: "echo".to_string(),
                               collection: None,
                           }
                        )),
                        target_name: Some("mynetstack2".to_string()),
                        availability: Some(fdecl::Availability::Optional),
                        ..Default::default()
                    }),
                    fdecl::Offer::Service(fdecl::OfferService {
                        source: Some(fdecl::Ref::Parent(fdecl::ParentRef {})),
                        source_name: Some("netstack3".to_string()),
                        target: Some(fdecl::Ref::Child(
                           fdecl::ChildRef {
                               name: "echo".to_string(),
                               collection: None,
                           }
                        )),
                        target_name: Some("mynetstack3".to_string()),
                        source_instance_filter: Some(vec!["allowedinstance".to_string()]),
                        renamed_instances: Some(vec![fdecl::NameMapping{source_name: "default".to_string(), target_name: "allowedinstance".to_string()}]),
                        availability: Some(fdecl::Availability::Required),
                        ..Default::default()
                    }),
                    fdecl::Offer::EventStream (
                        fdecl::OfferEventStream {
                            source: Some(fdecl::Ref::Parent(fdecl::ParentRef{})),
                            source_name: Some("diagnostics_ready".to_string()),
                            target: Some(fdecl::Ref::Child(fdecl::ChildRef {name: "netstack".to_string(), collection: None})),
                            scope: Some(vec![fdecl::Ref::Child(fdecl::ChildRef{name: "netstack".to_string(), collection: None})]),
                            target_name: Some("diagnostics_ready".to_string()),
                            availability: Some(fdecl::Availability::Optional),
                            ..Default::default()
                        }
                    )
                ]),
                capabilities: Some(vec![
                    fdecl::Capability::Service(fdecl::Service {
                        name: Some("netstack".to_string()),
                        source_path: Some("/netstack".to_string()),
                        ..Default::default()
                    }),
                    fdecl::Capability::Protocol(fdecl::Protocol {
                        name: Some("netstack2".to_string()),
                        source_path: Some("/netstack2".to_string()),
                        ..Default::default()
                    }),
                    fdecl::Capability::Directory(fdecl::Directory {
                        name: Some("data".to_string()),
                        source_path: Some("/data".to_string()),
                        rights: Some(fio::Operations::CONNECT),
                        ..Default::default()
                    }),
                    fdecl::Capability::Storage(fdecl::Storage {
                        name: Some("cache".to_string()),
                        backing_dir: Some("data".to_string()),
                        source: Some(fdecl::Ref::Parent(fdecl::ParentRef {})),
                        subdir: Some("cache".to_string()),
                        storage_id: Some(fdecl::StorageId::StaticInstanceId),
                        ..Default::default()
                    }),
                    fdecl::Capability::Runner(fdecl::Runner {
                        name: Some("elf".to_string()),
                        source_path: Some("/elf".to_string()),
                        ..Default::default()
                    }),
                    fdecl::Capability::Resolver(fdecl::Resolver {
                        name: Some("pkg".to_string()),
                        source_path: Some("/pkg_resolver".to_string()),
                        ..Default::default()
                    }),
                ]),
                children: Some(vec![
                     fdecl::Child {
                         name: Some("netstack".to_string()),
                         url: Some("fuchsia-pkg://fuchsia.com/netstack#meta/netstack.cm"
                                   .to_string()),
                         startup: Some(fdecl::StartupMode::Lazy),
                         on_terminate: None,
                         environment: None,
                         ..Default::default()
                     },
                     fdecl::Child {
                         name: Some("gtest".to_string()),
                         url: Some("fuchsia-pkg://fuchsia.com/gtest#meta/gtest.cm".to_string()),
                         startup: Some(fdecl::StartupMode::Lazy),
                         on_terminate: Some(fdecl::OnTerminate::None),
                         environment: None,
                         ..Default::default()
                     },
                     fdecl::Child {
                         name: Some("echo".to_string()),
                         url: Some("fuchsia-pkg://fuchsia.com/echo#meta/echo.cm"
                                   .to_string()),
                         startup: Some(fdecl::StartupMode::Eager),
                         on_terminate: Some(fdecl::OnTerminate::Reboot),
                         environment: Some("test_env".to_string()),
                         ..Default::default()
                     },
                ]),
                collections: Some(vec![
                     fdecl::Collection {
                         name: Some("modular".to_string()),
                         durability: Some(fdecl::Durability::Transient),
                         environment: None,
                         allowed_offers: Some(fdecl::AllowedOffers::StaticOnly),
                         allow_long_names: Some(true),
                         persistent_storage: None,
                         ..Default::default()
                     },
                     fdecl::Collection {
                         name: Some("tests".to_string()),
                         durability: Some(fdecl::Durability::Transient),
                         environment: Some("test_env".to_string()),
                         allowed_offers: Some(fdecl::AllowedOffers::StaticAndDynamic),
                         allow_long_names: Some(true),
                         persistent_storage: Some(true),
                         ..Default::default()
                     },
                ]),
                facets: Some(fdata::Dictionary {
                    entries: Some(vec![
                        fdata::DictionaryEntry {
                            key: "author".to_string(),
                            value: Some(Box::new(fdata::DictionaryValue::Str("Fuchsia".to_string()))),
                        },
                    ]),
                    ..Default::default()
                }),
                environments: Some(vec![
                    fdecl::Environment {
                        name: Some("test_env".to_string()),
                        extends: Some(fdecl::EnvironmentExtends::Realm),
                        runners: Some(vec![
                            fdecl::RunnerRegistration {
                                source_name: Some("runner".to_string()),
                                source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                                    name: "gtest".to_string(),
                                    collection: None,
                                })),
                                target_name: Some("gtest-runner".to_string()),
                                ..Default::default()
                            }
                        ]),
                        resolvers: Some(vec![
                            fdecl::ResolverRegistration {
                                resolver: Some("pkg_resolver".to_string()),
                                source: Some(fdecl::Ref::Parent(fdecl::ParentRef{})),
                                scheme: Some("fuchsia-pkg".to_string()),
                                ..Default::default()
                            }
                        ]),
                        debug_capabilities: Some(vec![
                         fdecl::DebugRegistration::Protocol(fdecl::DebugProtocolRegistration {
                             source_name: Some("some_protocol".to_string()),
                             source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                                 name: "gtest".to_string(),
                                 collection: None,
                             })),
                             target_name: Some("some_protocol".to_string()),
                             ..Default::default()
                            })
                        ]),
                        stop_timeout_ms: Some(4567),
                        ..Default::default()
                    }
                ]),
                config: Some(fdecl::ConfigSchema{
                    fields: Some(vec![
                        fdecl::ConfigField {
                            key: Some("enable_logging".to_string()),
                            type_: Some(fdecl::ConfigType {
                                layout: fdecl::ConfigTypeLayout::Bool,
                                parameters: Some(vec![]),
                                constraints: vec![],
                            }),
                            mutability: Some(Default::default()),
                            ..Default::default()
                        }
                    ]),
                    checksum: Some(fdecl::ConfigChecksum::Sha256([
                        0x64, 0x49, 0x9E, 0x75, 0xF3, 0x37, 0x69, 0x88, 0x74, 0x3B, 0x38, 0x16,
                        0xCD, 0x14, 0x70, 0x9F, 0x3D, 0x4A, 0xD3, 0xE2, 0x24, 0x9A, 0x1A, 0x34,
                        0x80, 0xB4, 0x9E, 0xB9, 0x63, 0x57, 0xD6, 0xED,
                    ])),
                    value_source: Some(
                        fdecl::ConfigValueSource::PackagePath("fake.cvf".to_string())
                    ),
                    ..Default::default()
                }),
                ..Default::default()
            },
            result = {
                ComponentDecl {
                    program: Some(ProgramDecl {
                        runner: Some("elf".try_into().unwrap()),
                        info: fdata::Dictionary {
                            entries: Some(vec![
                                fdata::DictionaryEntry {
                                    key: "args".to_string(),
                                    value: Some(Box::new(fdata::DictionaryValue::StrVec(vec!["foo".to_string(), "bar".to_string()]))),
                                },
                                fdata::DictionaryEntry{
                                    key: "binary".to_string(),
                                    value: Some(Box::new(fdata::DictionaryValue::Str("bin/app".to_string()))),
                                },
                            ]),
                            ..Default::default()
                        },
                    }),
                    uses: vec![
                        UseDecl::Service(UseServiceDecl {
                            dependency_type: DependencyType::Strong,
                            source: UseSource::Parent,
                            source_name: "netstack".try_into().unwrap(),
                            target_path: "/svc/mynetstack".try_into().unwrap(),
                            availability: Availability::Required,
                        }),
                        UseDecl::Protocol(UseProtocolDecl {
                            dependency_type: DependencyType::Strong,
                            source: UseSource::Parent,
                            source_name: "legacy_netstack".try_into().unwrap(),
                            target_path: "/svc/legacy_mynetstack".try_into().unwrap(),
                            availability: Availability::Optional,
                        }),
                        UseDecl::Protocol(UseProtocolDecl {
                            dependency_type: DependencyType::Strong,
                            source: UseSource::Child("echo".to_string()),
                            source_name: "echo_service".try_into().unwrap(),
                            target_path: "/svc/echo_service".try_into().unwrap(),
                            availability: Availability::Required,
                        }),
                        UseDecl::Directory(UseDirectoryDecl {
                            dependency_type: DependencyType::Strong,
                            source: UseSource::Framework,
                            source_name: "dir".try_into().unwrap(),
                            target_path: "/data".try_into().unwrap(),
                            rights: fio::Operations::CONNECT,
                            subdir: Some("foo/bar".into()),
                            availability: Availability::Required,
                        }),
                        UseDecl::Storage(UseStorageDecl {
                            source_name: "cache".into(),
                            target_path: "/cache".try_into().unwrap(),
                            availability: Availability::Required,
                        }),
                        UseDecl::Storage(UseStorageDecl {
                            source_name: "temp".into(),
                            target_path: "/temp".try_into().unwrap(),
                            availability: Availability::Optional,
                        }),
                        UseDecl::EventStream(UseEventStreamDecl {
                            source: UseSource::Child("test".to_string()),
                            scope: Some(vec![EventScope::Child(ChildRef{ name: "a".into(), collection: None}), EventScope::Collection("b".to_string())]),
                            source_name: CapabilityName::from("stopped"),
                            target_path: CapabilityPath::from_str("/svc/test").unwrap(),
                            filter: None,
                            availability: Availability::Optional,
                        }),
                    ],
                    exposes: vec![
                        ExposeDecl::Protocol(ExposeProtocolDecl {
                            source: ExposeSource::Child("netstack".to_string()),
                            source_name: "legacy_netstack".try_into().unwrap(),
                            target_name: "legacy_mynetstack".try_into().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: Availability::Required,
                        }),
                        ExposeDecl::Directory(ExposeDirectoryDecl {
                            source: ExposeSource::Child("netstack".to_string()),
                            source_name: "dir".try_into().unwrap(),
                            target_name: "data".try_into().unwrap(),
                            target: ExposeTarget::Parent,
                            rights: Some(fio::Operations::CONNECT),
                            subdir: Some("foo/bar".into()),
                            availability: Availability::Optional,
                        }),
                        ExposeDecl::Runner(ExposeRunnerDecl {
                            source: ExposeSource::Child("netstack".to_string()),
                            source_name: "elf".try_into().unwrap(),
                            target: ExposeTarget::Parent,
                            target_name: "elf".try_into().unwrap(),
                        }),
                        ExposeDecl::Resolver(ExposeResolverDecl {
                            source: ExposeSource::Child("netstack".to_string()),
                            source_name: "pkg".try_into().unwrap(),
                            target: ExposeTarget::Parent,
                            target_name: "pkg".try_into().unwrap(),
                        }),
                        ExposeDecl::Service(ExposeServiceDecl {
                            source: ExposeSource::Child("netstack".to_string()),
                            source_name: "netstack1".try_into().unwrap(),
                            target_name: "mynetstack".try_into().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: Availability::Required,
                        }),
                        ExposeDecl::Service(ExposeServiceDecl {
                            source: ExposeSource::Child("netstack".to_string()),
                            source_name: "netstack2".try_into().unwrap(),
                            target_name: "mynetstack".try_into().unwrap(),
                            target: ExposeTarget::Parent,
                            availability: Availability::Required,
                        }),
                    ],
                    offers: vec![
                        OfferDecl::Protocol(OfferProtocolDecl {
                            source: OfferSource::Parent,
                            source_name: "legacy_netstack".try_into().unwrap(),
                            target: OfferTarget::static_child("echo".to_string()),
                            target_name: "legacy_mynetstack".try_into().unwrap(),
                            dependency_type: DependencyType::WeakForMigration,
                            availability: Availability::Required,
                        }),
                        OfferDecl::Directory(OfferDirectoryDecl {
                            source: OfferSource::Parent,
                            source_name: "dir".try_into().unwrap(),
                            target: OfferTarget::Collection("modular".to_string()),
                            target_name: "data".try_into().unwrap(),
                            rights: Some(fio::Operations::CONNECT),
                            subdir: None,
                            dependency_type: DependencyType::Strong,
                            availability: Availability::Optional,
                        }),
                        OfferDecl::Storage(OfferStorageDecl {
                            source_name: "cache".try_into().unwrap(),
                            source: OfferSource::Self_,
                            target: OfferTarget::Collection("modular".to_string()),
                            target_name: "cache".try_into().unwrap(),
                            availability: Availability::Required,
                        }),
                        OfferDecl::Runner(OfferRunnerDecl {
                            source: OfferSource::Parent,
                            source_name: "elf".try_into().unwrap(),
                            target: OfferTarget::static_child("echo".to_string()),
                            target_name: "elf2".try_into().unwrap(),
                        }),
                        OfferDecl::Resolver(OfferResolverDecl {
                            source: OfferSource::Parent,
                            source_name: "pkg".try_into().unwrap(),
                            target: OfferTarget::static_child("echo".to_string()),
                            target_name: "pkg".try_into().unwrap(),
                        }),
                        OfferDecl::Service(OfferServiceDecl {
                                    source: OfferSource::Parent,
                                    source_name: "netstack1".try_into().unwrap(),
                                    source_instance_filter: None,
                                    renamed_instances: None,
                            target: OfferTarget::static_child("echo".to_string()),
                            target_name: "mynetstack1".try_into().unwrap(),
                            availability: Availability::Required,
                        }),
                        OfferDecl::Service(OfferServiceDecl {
                                    source: OfferSource::Parent,
                                    source_name: "netstack2".try_into().unwrap(),
                                    source_instance_filter: None,
                                    renamed_instances: None,
                            target: OfferTarget::static_child("echo".to_string()),
                            target_name: "mynetstack2".try_into().unwrap(),
                            availability: Availability::Optional,
                        }),
                        OfferDecl::Service(OfferServiceDecl {
                                    source: OfferSource::Parent,
                                    source_name: "netstack3".try_into().unwrap(),
                                    source_instance_filter: Some(vec!["allowedinstance".to_string()]),
                                    renamed_instances: Some(vec![NameMapping{source_name: "default".to_string(), target_name: "allowedinstance".to_string()}]),
                            target: OfferTarget::static_child("echo".to_string()),
                            target_name: "mynetstack3".try_into().unwrap(),
                            availability: Availability::Required,
                        }),
                        OfferDecl::EventStream (
                            OfferEventStreamDecl {
                                source: OfferSource::Parent,
                                source_name: CapabilityName::from("diagnostics_ready"),
                                target: OfferTarget::Child(ChildRef{name: "netstack".into(), collection: None}),
                                scope: Some(vec![EventScope::Child(ChildRef{ name: "netstack".into(), collection: None})]),
                                target_name: CapabilityName::from("diagnostics_ready"),
                                filter: None,
                                availability: Availability::Optional,
                            }
                        )
                    ],
                    capabilities: vec![
                        CapabilityDecl::Service(ServiceDecl {
                            name: "netstack".into(),
                            source_path: Some("/netstack".try_into().unwrap()),
                        }),
                        CapabilityDecl::Protocol(ProtocolDecl {
                            name: "netstack2".into(),
                            source_path: Some("/netstack2".try_into().unwrap()),
                        }),
                        CapabilityDecl::Directory(DirectoryDecl {
                            name: "data".into(),
                            source_path: Some("/data".try_into().unwrap()),
                            rights: fio::Operations::CONNECT,
                        }),
                        CapabilityDecl::Storage(StorageDecl {
                            name: "cache".into(),
                            backing_dir: "data".try_into().unwrap(),
                            source: StorageDirectorySource::Parent,
                            subdir: Some("cache".try_into().unwrap()),
                            storage_id: fdecl::StorageId::StaticInstanceId,
                        }),
                        CapabilityDecl::Runner(RunnerDecl {
                            name: "elf".into(),
                            source_path: Some("/elf".try_into().unwrap()),
                        }),
                        CapabilityDecl::Resolver(ResolverDecl {
                            name: "pkg".into(),
                            source_path: Some("/pkg_resolver".try_into().unwrap()),
                        }),
                    ],
                    children: vec![
                        ChildDecl {
                            name: "netstack".to_string(),
                            url: "fuchsia-pkg://fuchsia.com/netstack#meta/netstack.cm".to_string(),
                            startup: fdecl::StartupMode::Lazy,
                            on_terminate: None,
                            environment: None,
                            config_overrides: None,
                        },
                        ChildDecl {
                            name: "gtest".to_string(),
                            url: "fuchsia-pkg://fuchsia.com/gtest#meta/gtest.cm".to_string(),
                            startup: fdecl::StartupMode::Lazy,
                            on_terminate: Some(fdecl::OnTerminate::None),
                            environment: None,
                            config_overrides: None,
                        },
                        ChildDecl {
                            name: "echo".to_string(),
                            url: "fuchsia-pkg://fuchsia.com/echo#meta/echo.cm".to_string(),
                            startup: fdecl::StartupMode::Eager,
                            on_terminate: Some(fdecl::OnTerminate::Reboot),
                            environment: Some("test_env".to_string()),
                            config_overrides: None,
                        },
                    ],
                    collections: vec![
                        CollectionDecl {
                            name: "modular".to_string(),
                            durability: fdecl::Durability::Transient,
                            environment: None,
                            allowed_offers: cm_types::AllowedOffers::StaticOnly,
                            allow_long_names: true,
                            persistent_storage: None,
                        },
                        CollectionDecl {
                            name: "tests".to_string(),
                            durability: fdecl::Durability::Transient,
                            environment: Some("test_env".to_string()),
                            allowed_offers: cm_types::AllowedOffers::StaticAndDynamic,
                            allow_long_names: true,
                            persistent_storage: Some(true),
                        },
                    ],
                    facets: Some(fdata::Dictionary {
                        entries: Some(vec![
                            fdata::DictionaryEntry {
                                key: "author".to_string(),
                                value: Some(Box::new(fdata::DictionaryValue::Str("Fuchsia".to_string()))),
                            },
                        ]),
                        ..Default::default()
                    }),
                    environments: vec![
                        EnvironmentDecl {
                            name: "test_env".into(),
                            extends: fdecl::EnvironmentExtends::Realm,
                            runners: vec![
                                RunnerRegistration {
                                    source_name: "runner".into(),
                                    source: RegistrationSource::Child("gtest".to_string()),
                                    target_name: "gtest-runner".into(),
                                }
                            ],
                            resolvers: vec![
                                ResolverRegistration {
                                    resolver: "pkg_resolver".into(),
                                    source: RegistrationSource::Parent,
                                    scheme: "fuchsia-pkg".to_string(),
                                }
                            ],
                            debug_capabilities: vec![
                                DebugRegistration::Protocol(DebugProtocolRegistration {
                                    source_name: "some_protocol".into(),
                                    source: RegistrationSource::Child("gtest".to_string()),
                                    target_name: "some_protocol".into(),
                                })
                            ],
                            stop_timeout_ms: Some(4567),
                        }
                    ],
                    config: Some(ConfigDecl {
                        fields: vec![
                            ConfigField {
                                key: "enable_logging".to_string(),
                                type_: ConfigValueType::Bool,
                                mutability: ConfigMutability::default(),
                            }
                        ],
                        checksum: ConfigChecksum::Sha256([
                            0x64, 0x49, 0x9E, 0x75, 0xF3, 0x37, 0x69, 0x88, 0x74, 0x3B, 0x38, 0x16,
                            0xCD, 0x14, 0x70, 0x9F, 0x3D, 0x4A, 0xD3, 0xE2, 0x24, 0x9A, 0x1A, 0x34,
                            0x80, 0xB4, 0x9E, 0xB9, 0x63, 0x57, 0xD6, 0xED,
                        ]),
                        value_source: ConfigValueSource::PackagePath("fake.cvf".to_string())
                    }),
                }
            },
        },
    }

    test_capability_path! {
        capability_path_one_part => {
            input = "/foo",
            result = Ok(CapabilityPath{dirname: "/".into(), basename: "foo".into()}),
        },
        capability_path_two_parts => {
            input = "/foo/bar",
            result = Ok(CapabilityPath{dirname: "/foo".into(), basename: "bar".into()}),
        },
        capability_path_many_parts => {
            input = "/foo/bar/long/path",
            result = Ok(CapabilityPath{
                dirname: "/foo/bar/long".into(),
                basename: "path".into()
            }),
        },
        capability_path_invalid_empty_part => {
            input = "/foo/bar//long/path",
            result = Err(Error::InvalidCapabilityPath{raw: "/foo/bar//long/path".to_string()}),
        },
        capability_path_invalid_empty => {
            input = "",
            result = Err(Error::InvalidCapabilityPath{raw: "".to_string()}),
        },
        capability_path_invalid_root => {
            input = "/",
            result = Err(Error::InvalidCapabilityPath{raw: "/".to_string()}),
        },
        capability_path_invalid_relative => {
            input = "foo/bar",
            result = Err(Error::InvalidCapabilityPath{raw: "foo/bar".to_string()}),
        },
        capability_path_invalid_trailing => {
            input = "/foo/bar/",
            result = Err(Error::InvalidCapabilityPath{raw: "/foo/bar/".to_string()}),
        },
    }

    test_fidl_into_and_from! {
        fidl_into_and_from_use_source => {
            input = vec![
                fdecl::Ref::Parent(fdecl::ParentRef{}),
                fdecl::Ref::Framework(fdecl::FrameworkRef{}),
                fdecl::Ref::Debug(fdecl::DebugRef{}),
                fdecl::Ref::Capability(fdecl::CapabilityRef {name: "capability".to_string()}),
                fdecl::Ref::Child(fdecl::ChildRef {
                    name: "foo".into(),
                    collection: None,
                }),
            ],
            input_type = fdecl::Ref,
            result = vec![
                UseSource::Parent,
                UseSource::Framework,
                UseSource::Debug,
                UseSource::Capability(CapabilityName::from("capability")),
                UseSource::Child("foo".to_string()),
            ],
            result_type = UseSource,
        },
        fidl_into_and_from_expose_source => {
            input = vec![
                fdecl::Ref::Self_(fdecl::SelfRef {}),
                fdecl::Ref::Child(fdecl::ChildRef {
                    name: "foo".into(),
                    collection: None,
                }),
                fdecl::Ref::Framework(fdecl::FrameworkRef {}),
                fdecl::Ref::Collection(fdecl::CollectionRef { name: "foo".to_string() }),
            ],
            input_type = fdecl::Ref,
            result = vec![
                ExposeSource::Self_,
                ExposeSource::Child("foo".to_string()),
                ExposeSource::Framework,
                ExposeSource::Collection("foo".to_string()),
            ],
            result_type = ExposeSource,
        },
        fidl_into_and_from_offer_source => {
            input = vec![
                fdecl::Ref::Self_(fdecl::SelfRef {}),
                fdecl::Ref::Child(fdecl::ChildRef {
                    name: "foo".into(),
                    collection: None,
                }),
                fdecl::Ref::Framework(fdecl::FrameworkRef {}),
                fdecl::Ref::Capability(fdecl::CapabilityRef { name: "foo".to_string() }),
                fdecl::Ref::Parent(fdecl::ParentRef {}),
                fdecl::Ref::Collection(fdecl::CollectionRef { name: "foo".to_string() }),
                fdecl::Ref::VoidType(fdecl::VoidRef {}),
            ],
            input_type = fdecl::Ref,
            result = vec![
                OfferSource::Self_,
                OfferSource::static_child("foo".to_string()),
                OfferSource::Framework,
                OfferSource::Capability(CapabilityName::from("foo")),
                OfferSource::Parent,
                OfferSource::Collection("foo".to_string()),
                OfferSource::Void,
            ],
            result_type = OfferSource,
        },
        fidl_into_and_from_capability_without_path => {
            input = vec![
                fdecl::Protocol {
                    name: Some("foo_protocol".to_string()),
                    source_path: None,
                    ..Default::default()
                },
            ],
            input_type = fdecl::Protocol,
            result = vec![
                ProtocolDecl {
                    name: "foo_protocol".into(),
                    source_path: None,
                }
            ],
            result_type = ProtocolDecl,
        },
        fidl_into_and_from_storage_capability => {
            input = vec![
                fdecl::Storage {
                    name: Some("minfs".to_string()),
                    backing_dir: Some("minfs".into()),
                    source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                        name: "foo".into(),
                        collection: None,
                    })),
                    subdir: None,
                    storage_id: Some(fdecl::StorageId::StaticInstanceIdOrMoniker),
                    ..Default::default()
                },
            ],
            input_type = fdecl::Storage,
            result = vec![
                StorageDecl {
                    name: "minfs".into(),
                    backing_dir: "minfs".into(),
                    source: StorageDirectorySource::Child("foo".to_string()),
                    subdir: None,
                    storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
                },
            ],
            result_type = StorageDecl,
        },
        fidl_into_and_from_storage_capability_restricted => {
            input = vec![
                fdecl::Storage {
                    name: Some("minfs".to_string()),
                    backing_dir: Some("minfs".into()),
                    source: Some(fdecl::Ref::Child(fdecl::ChildRef {
                        name: "foo".into(),
                        collection: None,
                    })),
                    subdir: None,
                    storage_id: Some(fdecl::StorageId::StaticInstanceId),
                    ..Default::default()
                },
            ],
            input_type = fdecl::Storage,
            result = vec![
                StorageDecl {
                    name: "minfs".into(),
                    backing_dir: "minfs".into(),
                    source: StorageDirectorySource::Child("foo".to_string()),
                    subdir: None,
                    storage_id: fdecl::StorageId::StaticInstanceId,
                },
            ],
            result_type = StorageDecl,
        },
    }

    test_fidl_into! {
        all_with_omitted_defaults => {
            input = fdecl::Component {
                program: Some(fdecl::Program {
                    runner: Some("elf".to_string()),
                    info: Some(fdata::Dictionary {
                        entries: Some(vec![]),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                uses: Some(vec![]),
                exposes: Some(vec![]),
                offers: Some(vec![]),
                capabilities: Some(vec![]),
                children: Some(vec![]),
                collections: Some(vec![
                     fdecl::Collection {
                         name: Some("modular".to_string()),
                         durability: Some(fdecl::Durability::Transient),
                         environment: None,
                         allowed_offers: None,
                         allow_long_names: None,
                         persistent_storage: None,
                         ..Default::default()
                     },
                     fdecl::Collection {
                         name: Some("tests".to_string()),
                         durability: Some(fdecl::Durability::Transient),
                         environment: Some("test_env".to_string()),
                         allowed_offers: Some(fdecl::AllowedOffers::StaticOnly),
                         allow_long_names: None,
                         persistent_storage: Some(false),
                         ..Default::default()
                     },
                     fdecl::Collection {
                         name: Some("dyn_offers".to_string()),
                         durability: Some(fdecl::Durability::Transient),
                         allowed_offers: Some(fdecl::AllowedOffers::StaticAndDynamic),
                         allow_long_names: None,
                         persistent_storage: Some(true),
                         ..Default::default()
                     },
                 fdecl::Collection {
                         name: Some("long_child_names".to_string()),
                         durability: Some(fdecl::Durability::Transient),
                         allowed_offers: None,
                         allow_long_names: Some(true),
                         persistent_storage: None,
                         ..Default::default()
                     },
                ]),
                facets: Some(fdata::Dictionary{
                    entries: Some(vec![]),
                    ..Default::default()
                }),
                environments: Some(vec![]),
                ..Default::default()
            },
            result = {
                ComponentDecl {
                    program: Some(ProgramDecl {
                        runner: Some("elf".try_into().unwrap()),
                        info: fdata::Dictionary {
                            entries: Some(vec![]),
                            ..Default::default()
                        },
                    }),
                    uses: vec![],
                    exposes: vec![],
                    offers: vec![],
                    capabilities: vec![],
                    children: vec![],
                    collections: vec![
                        CollectionDecl {
                            name: "modular".to_string(),
                            durability: fdecl::Durability::Transient,
                            environment: None,
                            allowed_offers: cm_types::AllowedOffers::StaticOnly,
                            allow_long_names: false,
                            persistent_storage: None,
                        },
                        CollectionDecl {
                            name: "tests".to_string(),
                            durability: fdecl::Durability::Transient,
                            environment: Some("test_env".to_string()),
                            allowed_offers: cm_types::AllowedOffers::StaticOnly,
                            allow_long_names: false,
                            persistent_storage: Some(false),
                        },
                        CollectionDecl {
                            name: "dyn_offers".to_string(),
                            durability: fdecl::Durability::Transient,
                            environment: None,
                            allowed_offers: cm_types::AllowedOffers::StaticAndDynamic,
                            allow_long_names: false,
                            persistent_storage: Some(true),
                        },
                        CollectionDecl {
                            name: "long_child_names".to_string(),
                            durability: fdecl::Durability::Transient,
                            environment: None,
                            allowed_offers: cm_types::AllowedOffers::StaticOnly,
                            allow_long_names: true,
                            persistent_storage: None,
                        },
                    ],
                    facets: Some(fdata::Dictionary{
                        entries: Some(vec![]),
                        ..Default::default()
                    }),
                    environments: vec![],
                    config: None,
                }
            },
        },
    }

    #[test]
    fn default_expose_availability() {
        let source = fdecl::Ref::Self_(fdecl::SelfRef {});
        let source_name = "";
        let target = fdecl::Ref::Parent(fdecl::ParentRef {});
        let target_name = "";
        assert_eq!(
            *fdecl::ExposeService {
                source: Some(source.clone()),
                source_name: Some(source_name.into()),
                target: Some(target.clone()),
                target_name: Some(target_name.into()),
                availability: None,
                ..Default::default()
            }
            .fidl_into_native()
            .availability(),
            Availability::Required
        );
        assert_eq!(
            *fdecl::ExposeProtocol {
                source: Some(source.clone()),
                source_name: Some(source_name.into()),
                target: Some(target.clone()),
                target_name: Some(target_name.into()),
                ..Default::default()
            }
            .fidl_into_native()
            .availability(),
            Availability::Required
        );
        assert_eq!(
            *fdecl::ExposeDirectory {
                source: Some(source.clone()),
                source_name: Some(source_name.into()),
                target: Some(target.clone()),
                target_name: Some(target_name.into()),
                ..Default::default()
            }
            .fidl_into_native()
            .availability(),
            Availability::Required
        );
        assert_eq!(
            *fdecl::ExposeRunner {
                source: Some(source.clone()),
                source_name: Some(source_name.into()),
                target: Some(target.clone()),
                target_name: Some(target_name.into()),
                ..Default::default()
            }
            .fidl_into_native()
            .availability(),
            Availability::Required
        );
        assert_eq!(
            *fdecl::ExposeResolver {
                source: Some(source.clone()),
                source_name: Some(source_name.into()),
                target: Some(target.clone()),
                target_name: Some(target_name.into()),
                ..Default::default()
            }
            .fidl_into_native()
            .availability(),
            Availability::Required
        );
    }
}
