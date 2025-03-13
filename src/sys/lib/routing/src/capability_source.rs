// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::error::RoutingError;
use crate::legacy_router::Sources;
use cm_rust::{
    CapabilityDecl, CapabilityTypeName, ChildRef, ConfigurationDecl, DictionaryDecl, DirectoryDecl,
    EventStreamDecl, ExposeConfigurationDecl, ExposeDecl, ExposeDeclCommon, ExposeDictionaryDecl,
    ExposeDirectoryDecl, ExposeProtocolDecl, ExposeResolverDecl, ExposeRunnerDecl,
    ExposeServiceDecl, ExposeSource, FidlIntoNative, NameMapping, NativeIntoFidl,
    OfferConfigurationDecl, OfferDecl, OfferDeclCommon, OfferDictionaryDecl, OfferDirectoryDecl,
    OfferEventStreamDecl, OfferProtocolDecl, OfferResolverDecl, OfferRunnerDecl, OfferServiceDecl,
    OfferSource, OfferStorageDecl, ProtocolDecl, RegistrationSource, ResolverDecl, RunnerDecl,
    ServiceDecl, StorageDecl, UseDecl, UseDeclCommon, UseDirectoryDecl, UseProtocolDecl,
    UseServiceDecl, UseSource, UseStorageDecl,
};
use cm_rust_derive::FidlDecl;
use cm_types::{Name, Path};
use derivative::Derivative;
use fidl::{persist, unpersist};
use from_enum::FromEnum;
use futures::future::BoxFuture;
use moniker::{ChildName, ExtendedMoniker, Moniker};
use sandbox::{Capability, Data};
use std::fmt;
use thiserror::Error;
use {
    fidl_fuchsia_component_decl as fdecl, fidl_fuchsia_component_internal as finternal,
    fidl_fuchsia_sys2 as fsys,
};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

#[derive(Debug, Error)]
pub enum Error {
    #[error("Invalid framework capability.")]
    InvalidFrameworkCapability {},
    #[error("Invalid builtin capability.")]
    InvalidBuiltinCapability {},
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(Debug, Hash, PartialEq, Eq, Clone)]
pub enum AggregateMember {
    Child(ChildRef),
    Collection(Name),
    Parent,
    Self_,
}

impl TryFrom<&cm_rust::OfferDecl> for AggregateMember {
    type Error = ();

    fn try_from(offer: &cm_rust::OfferDecl) -> Result<AggregateMember, ()> {
        match offer.source() {
            // TODO: should we panic instead of filtering when we find something we don't expect?
            cm_rust::OfferSource::Framework => Err(()),
            cm_rust::OfferSource::Parent => Ok(AggregateMember::Parent),
            cm_rust::OfferSource::Child(child) => Ok(AggregateMember::Child(child.clone())),
            cm_rust::OfferSource::Collection(name) => Ok(AggregateMember::Collection(name.clone())),
            cm_rust::OfferSource::Self_ => Ok(AggregateMember::Self_),
            cm_rust::OfferSource::Capability(_name) => Err(()),
            cm_rust::OfferSource::Void => Err(()),
        }
    }
}

impl TryFrom<&cm_rust::ExposeDecl> for AggregateMember {
    type Error = ();

    fn try_from(expose: &cm_rust::ExposeDecl) -> Result<AggregateMember, ()> {
        match expose.source() {
            // TODO: should we panic instead of filtering when we find something we don't expect?
            cm_rust::ExposeSource::Framework => Err(()),
            cm_rust::ExposeSource::Child(child) => Ok(AggregateMember::Child(cm_rust::ChildRef {
                name: child.clone().into(),
                collection: None,
            })),
            cm_rust::ExposeSource::Collection(name) => {
                Ok(AggregateMember::Collection(name.clone()))
            }
            cm_rust::ExposeSource::Self_ => Ok(AggregateMember::Self_),
            cm_rust::ExposeSource::Capability(_name) => Err(()),
            cm_rust::ExposeSource::Void => Err(()),
        }
    }
}

impl TryFrom<&cm_rust::UseDecl> for AggregateMember {
    type Error = ();

    fn try_from(use_: &cm_rust::UseDecl) -> Result<AggregateMember, ()> {
        match use_.source() {
            cm_rust::UseSource::Parent => Ok(AggregateMember::Parent),
            cm_rust::UseSource::Framework => Err(()),
            cm_rust::UseSource::Debug => Err(()),
            cm_rust::UseSource::Self_ => Ok(AggregateMember::Self_),
            cm_rust::UseSource::Capability(_) => Err(()),
            cm_rust::UseSource::Child(name) => {
                Ok(AggregateMember::Child(ChildRef { name: name.clone().into(), collection: None }))
            }
            cm_rust::UseSource::Collection(name) => Ok(AggregateMember::Collection(name.clone())),
            cm_rust::UseSource::Environment => Err(()),
        }
    }
}

impl fmt::Display for AggregateMember {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Child(n) => {
                write!(f, "child `{n}`")
            }
            Self::Collection(n) => {
                write!(f, "collection `{n}`")
            }
            Self::Parent => {
                write!(f, "parent")
            }
            Self::Self_ => {
                write!(f, "self")
            }
        }
    }
}

impl FidlIntoNative<AggregateMember> for finternal::AggregateMember {
    fn fidl_into_native(self) -> AggregateMember {
        match self {
            finternal::AggregateMember::Self_(_) => AggregateMember::Self_,
            finternal::AggregateMember::Parent(_) => AggregateMember::Parent,
            finternal::AggregateMember::Collection(name) => {
                AggregateMember::Collection(Name::new(name).unwrap())
            }
            finternal::AggregateMember::Child(child_ref) => {
                AggregateMember::Child(child_ref.fidl_into_native())
            }
        }
    }
}

impl NativeIntoFidl<finternal::AggregateMember> for AggregateMember {
    fn native_into_fidl(self) -> finternal::AggregateMember {
        match self {
            AggregateMember::Self_ => finternal::AggregateMember::Self_(fdecl::SelfRef {}),
            AggregateMember::Parent => finternal::AggregateMember::Parent(fdecl::ParentRef {}),
            AggregateMember::Collection(name) => {
                finternal::AggregateMember::Collection(name.to_string())
            }
            AggregateMember::Child(child_ref) => {
                finternal::AggregateMember::Child(child_ref.native_into_fidl())
            }
        }
    }
}

/// Describes the source of a capability, as determined by `find_capability_source`
#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(tag = "type", rename_all = "snake_case")
)]
#[derive(FidlDecl, Debug, Derivative)]
#[derivative(Clone(bound = ""), PartialEq)]
#[fidl_decl(fidl_union = "finternal::CapabilitySource")]
pub enum CapabilitySource {
    /// This capability originates from the component instance for the given Realm.
    /// point.
    Component(ComponentSource),
    /// This capability originates from "framework". It's implemented by component manager and is
    /// scoped to the realm of the source.
    Framework(FrameworkSource),
    /// This capability originates from the parent of the root component, and is built in to
    /// component manager. `top_instance` is the instance at the top of the tree, i.e.  the
    /// instance representing component manager.
    Builtin(BuiltinSource),
    /// This capability originates from the parent of the root component, and is offered from
    /// component manager's namespace. `top_instance` is the instance at the top of the tree, i.e.
    /// the instance representing component manager.
    Namespace(NamespaceSource),
    /// This capability is provided by the framework based on some other capability.
    Capability(CapabilityToCapabilitySource),
    /// This capability is an aggregate of capabilities over a set of collections and static
    /// children. The instance names in the aggregate service will be anonymized.
    AnonymizedAggregate(AnonymizedAggregateSource),
    /// This capability is a filtered service capability from a single source, such as self or a
    /// child.
    FilteredProvider(FilteredProviderSource),
    /// This capability is a filtered service capability with multiple sources, such as all of the
    /// dynamic children in a collection. The instances in the aggregate service are the union of
    /// the filters.
    FilteredAggregateProvider(FilteredAggregateProviderSource),
    /// This capability originates from "environment". It's implemented by a component instance.
    Environment(EnvironmentSource),
    /// This capability originates from "void". This is only a valid origination for optional
    /// capabilities.
    Void(VoidSource),
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, PartialEq, Clone)]
#[fidl_decl(fidl_table = "finternal::Component")]
pub struct ComponentSource {
    pub capability: ComponentCapability,
    pub moniker: Moniker,
}
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, PartialEq, Clone)]
#[fidl_decl(fidl_table = "finternal::Framework")]
pub struct FrameworkSource {
    pub capability: InternalCapability,
    pub moniker: Moniker,
}
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, PartialEq, Clone)]
#[fidl_decl(fidl_table = "finternal::Builtin")]
pub struct BuiltinSource {
    pub capability: InternalCapability,
}
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, PartialEq, Clone)]
#[fidl_decl(fidl_table = "finternal::Namespace")]
pub struct NamespaceSource {
    pub capability: ComponentCapability,
}
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, PartialEq, Clone)]
#[fidl_decl(fidl_table = "finternal::Capability")]
pub struct CapabilityToCapabilitySource {
    pub source_capability: ComponentCapability,
    pub moniker: Moniker,
}
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, PartialEq, Clone)]
#[fidl_decl(fidl_table = "finternal::AnonymizedAggregate")]
pub struct AnonymizedAggregateSource {
    pub capability: AggregateCapability,
    pub moniker: Moniker,
    pub members: Vec<AggregateMember>,
    pub sources: Sources,
    pub instances: Vec<ServiceInstance>,
}
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, PartialEq, Clone)]
#[fidl_decl(fidl_table = "fsys::ServiceInstance")]
pub struct ServiceInstance {
    pub instance_name: Name,
    pub child_name: String,
    pub child_instance_name: Name,
}
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, PartialEq, Clone)]
#[fidl_decl(fidl_table = "finternal::FilteredProvider")]
pub struct FilteredProviderSource {
    pub capability: AggregateCapability,
    pub moniker: Moniker,
    pub service_capability: ComponentCapability,
    pub offer_service_decl: OfferServiceDecl,
}
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, PartialEq, Clone)]
#[fidl_decl(fidl_table = "finternal::FilteredAggregateProvider")]
pub struct FilteredAggregateProviderSource {
    pub capability: AggregateCapability,
    pub moniker: Moniker,
    pub offer_service_decls: Vec<OfferServiceDecl>,
    pub sources: Sources,
}
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, PartialEq, Clone)]
#[fidl_decl(fidl_table = "finternal::Environment")]
pub struct EnvironmentSource {
    pub capability: ComponentCapability,
    pub moniker: Moniker,
}
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Debug, PartialEq, Clone)]
#[fidl_decl(fidl_table = "finternal::Void")]
pub struct VoidSource {
    pub capability: InternalCapability,
    pub moniker: Moniker,
}

impl CapabilitySource {
    /// Returns whether the given CapabilitySourceInterface can be available in a component's
    /// namespace.
    pub fn can_be_in_namespace(&self) -> bool {
        match self {
            Self::Component(ComponentSource { capability, .. }) => capability.can_be_in_namespace(),
            Self::Framework(FrameworkSource { capability, .. }) => capability.can_be_in_namespace(),
            Self::Builtin(BuiltinSource { capability }) => capability.can_be_in_namespace(),
            Self::Namespace(NamespaceSource { capability }) => capability.can_be_in_namespace(),
            Self::Capability(CapabilityToCapabilitySource { .. }) => true,
            Self::AnonymizedAggregate(AnonymizedAggregateSource { capability, .. }) => {
                capability.can_be_in_namespace()
            }
            Self::FilteredProvider(FilteredProviderSource { capability, .. })
            | Self::FilteredAggregateProvider(FilteredAggregateProviderSource {
                capability, ..
            }) => capability.can_be_in_namespace(),
            Self::Environment(EnvironmentSource { capability, .. }) => {
                capability.can_be_in_namespace()
            }
            Self::Void(VoidSource { capability, .. }) => capability.can_be_in_namespace(),
        }
    }

    pub fn source_name(&self) -> Option<&Name> {
        match self {
            Self::Component(ComponentSource { capability, .. }) => capability.source_name(),
            Self::Framework(FrameworkSource { capability, .. }) => Some(capability.source_name()),
            Self::Builtin(BuiltinSource { capability }) => Some(capability.source_name()),
            Self::Namespace(NamespaceSource { capability }) => capability.source_name(),
            Self::Capability(CapabilityToCapabilitySource { .. }) => None,
            Self::AnonymizedAggregate(AnonymizedAggregateSource { capability, .. }) => {
                Some(capability.source_name())
            }
            Self::FilteredProvider(FilteredProviderSource { capability, .. })
            | Self::FilteredAggregateProvider(FilteredAggregateProviderSource {
                capability, ..
            }) => Some(capability.source_name()),
            Self::Environment(EnvironmentSource { capability, .. }) => capability.source_name(),
            Self::Void(VoidSource { capability, .. }) => Some(capability.source_name()),
        }
    }

    pub fn type_name(&self) -> CapabilityTypeName {
        match self {
            Self::Component(ComponentSource { capability, .. }) => capability.type_name(),
            Self::Framework(FrameworkSource { capability, .. }) => capability.type_name(),
            Self::Builtin(BuiltinSource { capability }) => capability.type_name(),
            Self::Namespace(NamespaceSource { capability }) => capability.type_name(),
            Self::Capability(CapabilityToCapabilitySource { source_capability, .. }) => {
                source_capability.type_name()
            }
            Self::AnonymizedAggregate(AnonymizedAggregateSource { capability, .. }) => {
                capability.type_name()
            }
            Self::FilteredProvider(FilteredProviderSource { capability, .. })
            | Self::FilteredAggregateProvider(FilteredAggregateProviderSource {
                capability, ..
            }) => capability.type_name(),
            Self::Environment(EnvironmentSource { capability, .. }) => capability.type_name(),
            Self::Void(VoidSource { capability, .. }) => capability.type_name(),
        }
    }

    pub fn source_moniker(&self) -> ExtendedMoniker {
        match self {
            Self::Component(ComponentSource { moniker, .. })
            | Self::Framework(FrameworkSource { moniker, .. })
            | Self::Capability(CapabilityToCapabilitySource { moniker, .. })
            | Self::Environment(EnvironmentSource { moniker, .. })
            | Self::Void(VoidSource { moniker, .. })
            | Self::AnonymizedAggregate(AnonymizedAggregateSource { moniker, .. })
            | Self::FilteredProvider(FilteredProviderSource { moniker, .. })
            | Self::FilteredAggregateProvider(FilteredAggregateProviderSource {
                moniker, ..
            }) => ExtendedMoniker::ComponentInstance(moniker.clone()),
            Self::Builtin(_) | Self::Namespace(_) => ExtendedMoniker::ComponentManager,
        }
    }
}

impl fmt::Display for CapabilitySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Component(ComponentSource { capability, moniker }) => {
                    format!("{} '{}'", capability, moniker)
                }
                Self::Framework(FrameworkSource { capability, .. }) => capability.to_string(),
                Self::Builtin(BuiltinSource { capability }) => capability.to_string(),
                Self::Namespace(NamespaceSource { capability }) => capability.to_string(),
                Self::FilteredProvider(FilteredProviderSource { capability, .. })
                | Self::FilteredAggregateProvider(FilteredAggregateProviderSource {
                    capability,
                    ..
                }) => capability.to_string(),
                Self::Capability(CapabilityToCapabilitySource { source_capability, .. }) =>
                    format!("{}", source_capability),
                Self::AnonymizedAggregate(AnonymizedAggregateSource {
                    capability,
                    members,
                    moniker,
                    ..
                }) => {
                    format!(
                        "{} from component '{}' aggregated from {}",
                        capability,
                        moniker,
                        members.iter().map(|s| format!("{s}")).collect::<Vec<_>>().join(","),
                    )
                }
                Self::Environment(EnvironmentSource { capability, .. }) => capability.to_string(),
                Self::Void(VoidSource { capability, .. }) => capability.to_string(),
            }
        )
    }
}

impl TryFrom<CapabilitySource> for Capability {
    type Error = fidl::Error;

    fn try_from(capability_source: CapabilitySource) -> Result<Self, Self::Error> {
        Ok(Data::try_from(capability_source)?.into())
    }
}

impl TryFrom<CapabilitySource> for Data {
    type Error = fidl::Error;

    fn try_from(capability_source: CapabilitySource) -> Result<Self, Self::Error> {
        Ok(Data::Bytes(persist(&capability_source.native_into_fidl())?))
    }
}

impl TryFrom<Capability> for CapabilitySource {
    type Error = fidl::Error;

    fn try_from(capability: Capability) -> Result<Self, Self::Error> {
        let Capability::Data(data) = capability else {
            return Err(fidl::Error::InvalidEnumValue);
        };
        Self::try_from(data)
    }
}

impl TryFrom<Data> for CapabilitySource {
    type Error = fidl::Error;

    fn try_from(data: Data) -> Result<Self, Self::Error> {
        let Data::Bytes(bytes) = data else {
            return Err(fidl::Error::InvalidEnumValue);
        };
        Ok(unpersist::<finternal::CapabilitySource>(&bytes)?.fidl_into_native())
    }
}

/// An individual instance in an aggregate.
#[derive(Debug, Clone, Hash, PartialEq, Eq, PartialOrd, Ord)]
pub enum AggregateInstance {
    Child(ChildName),
    Parent,
    Self_,
}

impl fmt::Display for AggregateInstance {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Child(n) => {
                write!(f, "child `{n}`")
            }
            Self::Parent => {
                write!(f, "parent")
            }
            Self::Self_ => {
                write!(f, "self")
            }
        }
    }
}

/// The return value of the routing future returned by
/// `FilteredAggregateCapabilityProvider::route_instances`, which contains information about the
/// source of the route.
#[derive(Debug)]
pub struct FilteredAggregateCapabilityRouteData {
    /// The source of the capability.
    pub capability_source: CapabilitySource,
    /// The filter to apply to service instances, as defined by
    /// [`fuchsia.component.decl/OfferService.renamed_instances`](https://fuchsia.dev/reference/fidl/fuchsia.component.decl#OfferService).
    pub instance_filter: Vec<NameMapping>,
}

/// A provider of a capability from an aggregation of zero or more offered instances of a
/// capability, with filters.
///
/// This trait type-erases the capability type, so it can be handled and hosted generically.
pub trait FilteredAggregateCapabilityProvider: Send + Sync {
    /// Return a list of futures to route every instance in the aggregate to its source. Each
    /// result is paired with the list of instances to include in the source.
    fn route_instances(
        &self,
    ) -> Vec<BoxFuture<'_, Result<FilteredAggregateCapabilityRouteData, RoutingError>>>;

    /// Trait-object compatible clone.
    fn clone_boxed(&self) -> Box<dyn FilteredAggregateCapabilityProvider>;
}

impl Clone for Box<dyn FilteredAggregateCapabilityProvider> {
    fn clone(&self) -> Self {
        self.clone_boxed()
    }
}

impl fmt::Debug for Box<dyn FilteredAggregateCapabilityProvider> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Box<dyn FilteredAggregateCapabilityProvider>").finish()
    }
}

/// Describes a capability provided by the component manager which could be a framework capability
/// scoped to a realm, a built-in global capability, or a capability from component manager's own
/// namespace.
#[cfg_attr(feature = "serde", derive(Deserialize, Serialize), serde(rename_all = "snake_case"))]
#[derive(FidlDecl, Clone, Debug, PartialEq, Eq)]
#[fidl_decl(fidl_union = "finternal::InternalCapability")]
pub enum InternalCapability {
    Service(Name),
    Protocol(Name),
    Directory(Name),
    Runner(Name),
    Config(Name),
    EventStream(Name),
    Resolver(Name),
    Storage(Name),
    Dictionary(Name),
}

impl InternalCapability {
    pub fn new(type_name: CapabilityTypeName, name: Name) -> Self {
        match type_name {
            CapabilityTypeName::Directory => InternalCapability::Directory(name),
            CapabilityTypeName::EventStream => InternalCapability::Directory(name),
            CapabilityTypeName::Protocol => InternalCapability::Protocol(name),
            CapabilityTypeName::Resolver => InternalCapability::Resolver(name),
            CapabilityTypeName::Runner => InternalCapability::Runner(name),
            CapabilityTypeName::Service => InternalCapability::Service(name),
            CapabilityTypeName::Storage => InternalCapability::Storage(name),
            CapabilityTypeName::Dictionary => InternalCapability::Dictionary(name),
            CapabilityTypeName::Config => InternalCapability::Config(name),
        }
    }

    /// Returns whether the given InternalCapability can be available in a component's namespace.
    pub fn can_be_in_namespace(&self) -> bool {
        matches!(
            self,
            InternalCapability::Service(_)
                | InternalCapability::Protocol(_)
                | InternalCapability::Directory(_)
        )
    }

    /// Returns a name for the capability type.
    pub fn type_name(&self) -> CapabilityTypeName {
        match self {
            InternalCapability::Service(_) => CapabilityTypeName::Service,
            InternalCapability::Protocol(_) => CapabilityTypeName::Protocol,
            InternalCapability::Directory(_) => CapabilityTypeName::Directory,
            InternalCapability::Runner(_) => CapabilityTypeName::Runner,
            InternalCapability::Config(_) => CapabilityTypeName::Config,
            InternalCapability::EventStream(_) => CapabilityTypeName::EventStream,
            InternalCapability::Resolver(_) => CapabilityTypeName::Resolver,
            InternalCapability::Storage(_) => CapabilityTypeName::Storage,
            InternalCapability::Dictionary(_) => CapabilityTypeName::Dictionary,
        }
    }

    pub fn source_name(&self) -> &Name {
        match self {
            InternalCapability::Service(name) => &name,
            InternalCapability::Protocol(name) => &name,
            InternalCapability::Directory(name) => &name,
            InternalCapability::Runner(name) => &name,
            InternalCapability::Config(name) => &name,
            InternalCapability::EventStream(name) => &name,
            InternalCapability::Resolver(name) => &name,
            InternalCapability::Storage(name) => &name,
            InternalCapability::Dictionary(name) => &name,
        }
    }

    /// Returns true if this is a protocol with name that matches `name`.
    pub fn matches_protocol(&self, name: &Name) -> bool {
        match self {
            Self::Protocol(source_name) => source_name == name,
            _ => false,
        }
    }
}

impl fmt::Display for InternalCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} '{}' from component manager", self.type_name(), self.source_name())
    }
}

impl From<CapabilityDecl> for InternalCapability {
    fn from(capability: CapabilityDecl) -> Self {
        match capability {
            CapabilityDecl::Service(c) => InternalCapability::Service(c.name),
            CapabilityDecl::Protocol(c) => InternalCapability::Protocol(c.name),
            CapabilityDecl::Directory(c) => InternalCapability::Directory(c.name),
            CapabilityDecl::Storage(c) => InternalCapability::Storage(c.name),
            CapabilityDecl::Runner(c) => InternalCapability::Runner(c.name),
            CapabilityDecl::Resolver(c) => InternalCapability::Resolver(c.name),
            CapabilityDecl::EventStream(c) => InternalCapability::EventStream(c.name),
            CapabilityDecl::Dictionary(c) => InternalCapability::Dictionary(c.name),
            CapabilityDecl::Config(c) => InternalCapability::Config(c.name),
        }
    }
}

impl From<ServiceDecl> for InternalCapability {
    fn from(service: ServiceDecl) -> Self {
        Self::Service(service.name)
    }
}

impl From<ProtocolDecl> for InternalCapability {
    fn from(protocol: ProtocolDecl) -> Self {
        Self::Protocol(protocol.name)
    }
}

impl From<DirectoryDecl> for InternalCapability {
    fn from(directory: DirectoryDecl) -> Self {
        Self::Directory(directory.name)
    }
}

impl From<RunnerDecl> for InternalCapability {
    fn from(runner: RunnerDecl) -> Self {
        Self::Runner(runner.name)
    }
}

impl From<ResolverDecl> for InternalCapability {
    fn from(resolver: ResolverDecl) -> Self {
        Self::Resolver(resolver.name)
    }
}

impl From<EventStreamDecl> for InternalCapability {
    fn from(event: EventStreamDecl) -> Self {
        Self::EventStream(event.name)
    }
}

impl From<StorageDecl> for InternalCapability {
    fn from(storage: StorageDecl) -> Self {
        Self::Storage(storage.name)
    }
}

impl From<cm_rust::ConfigurationDecl> for InternalCapability {
    fn from(config: cm_rust::ConfigurationDecl) -> Self {
        Self::Config(config.name)
    }
}

/// A capability being routed from a component.
#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(tag = "type", rename_all = "snake_case")
)]
#[derive(FidlDecl, FromEnum, Clone, Debug, PartialEq, Eq)]
#[fidl_decl(fidl_union = "finternal::ComponentCapability")]
pub enum ComponentCapability {
    Use_(UseDecl),
    /// Models a capability used from the environment.
    Environment(EnvironmentCapability),
    Expose(ExposeDecl),
    Offer(OfferDecl),
    Protocol(ProtocolDecl),
    Directory(DirectoryDecl),
    Storage(StorageDecl),
    Runner(RunnerDecl),
    Resolver(ResolverDecl),
    Service(ServiceDecl),
    EventStream(EventStreamDecl),
    Dictionary(DictionaryDecl),
    Config(ConfigurationDecl),
}

impl ComponentCapability {
    /// Returns whether the given ComponentCapability can be available in a component's namespace.
    pub fn can_be_in_namespace(&self) -> bool {
        match self {
            ComponentCapability::Use_(use_) => {
                matches!(use_, UseDecl::Protocol(_) | UseDecl::Directory(_) | UseDecl::Service(_))
            }
            ComponentCapability::Expose(expose) => matches!(
                expose,
                ExposeDecl::Protocol(_) | ExposeDecl::Directory(_) | ExposeDecl::Service(_)
            ),
            ComponentCapability::Offer(offer) => matches!(
                offer,
                OfferDecl::Protocol(_) | OfferDecl::Directory(_) | OfferDecl::Service(_)
            ),
            ComponentCapability::Protocol(_)
            | ComponentCapability::Directory(_)
            | ComponentCapability::Service(_) => true,
            _ => false,
        }
    }

    /// Returns a name for the capability type.
    pub fn type_name(&self) -> CapabilityTypeName {
        match self {
            ComponentCapability::Use_(use_) => use_.into(),
            ComponentCapability::Environment(env) => match env {
                EnvironmentCapability::Runner(_) => CapabilityTypeName::Runner,
                EnvironmentCapability::Resolver(_) => CapabilityTypeName::Resolver,
                EnvironmentCapability::Debug(_) => CapabilityTypeName::Protocol,
            },
            ComponentCapability::Expose(expose) => expose.into(),
            ComponentCapability::Offer(offer) => offer.into(),
            ComponentCapability::Protocol(_) => CapabilityTypeName::Protocol,
            ComponentCapability::Directory(_) => CapabilityTypeName::Directory,
            ComponentCapability::Storage(_) => CapabilityTypeName::Storage,
            ComponentCapability::Runner(_) => CapabilityTypeName::Runner,
            ComponentCapability::Config(_) => CapabilityTypeName::Config,
            ComponentCapability::Resolver(_) => CapabilityTypeName::Resolver,
            ComponentCapability::Service(_) => CapabilityTypeName::Service,
            ComponentCapability::EventStream(_) => CapabilityTypeName::EventStream,
            ComponentCapability::Dictionary(_) => CapabilityTypeName::Dictionary,
        }
    }

    /// Return the source path of the capability, if one exists.
    pub fn source_path(&self) -> Option<&Path> {
        match self {
            ComponentCapability::Storage(_) => None,
            ComponentCapability::Protocol(protocol) => protocol.source_path.as_ref(),
            ComponentCapability::Directory(directory) => directory.source_path.as_ref(),
            ComponentCapability::Runner(runner) => runner.source_path.as_ref(),
            ComponentCapability::Resolver(resolver) => resolver.source_path.as_ref(),
            ComponentCapability::Service(service) => service.source_path.as_ref(),
            _ => None,
        }
    }

    /// Return the name of the capability, if this is a capability declaration.
    pub fn source_name(&self) -> Option<&Name> {
        match self {
            ComponentCapability::Storage(storage) => Some(&storage.name),
            ComponentCapability::Protocol(protocol) => Some(&protocol.name),
            ComponentCapability::Directory(directory) => Some(&directory.name),
            ComponentCapability::Runner(runner) => Some(&runner.name),
            ComponentCapability::Config(config) => Some(&config.name),
            ComponentCapability::Resolver(resolver) => Some(&resolver.name),
            ComponentCapability::Service(service) => Some(&service.name),
            ComponentCapability::EventStream(event) => Some(&event.name),
            ComponentCapability::Dictionary(dictionary) => Some(&dictionary.name),
            ComponentCapability::Use_(use_) => match use_ {
                UseDecl::Protocol(UseProtocolDecl { source_name, .. }) => Some(source_name),
                UseDecl::Directory(UseDirectoryDecl { source_name, .. }) => Some(source_name),
                UseDecl::Storage(UseStorageDecl { source_name, .. }) => Some(source_name),
                UseDecl::Service(UseServiceDecl { source_name, .. }) => Some(source_name),
                UseDecl::Config(cm_rust::UseConfigurationDecl { source_name, .. }) => {
                    Some(source_name)
                }
                _ => None,
            },
            ComponentCapability::Environment(env_cap) => match env_cap {
                EnvironmentCapability::Runner(EnvironmentCapabilityData {
                    source_name, ..
                }) => Some(source_name),
                EnvironmentCapability::Resolver(EnvironmentCapabilityData {
                    source_name, ..
                }) => Some(source_name),
                EnvironmentCapability::Debug(EnvironmentCapabilityData { source_name, .. }) => {
                    Some(source_name)
                }
            },
            ComponentCapability::Expose(expose) => match expose {
                ExposeDecl::Protocol(ExposeProtocolDecl { source_name, .. }) => Some(source_name),
                ExposeDecl::Directory(ExposeDirectoryDecl { source_name, .. }) => Some(source_name),
                ExposeDecl::Runner(ExposeRunnerDecl { source_name, .. }) => Some(source_name),
                ExposeDecl::Resolver(ExposeResolverDecl { source_name, .. }) => Some(source_name),
                ExposeDecl::Service(ExposeServiceDecl { source_name, .. }) => Some(source_name),
                ExposeDecl::Config(ExposeConfigurationDecl { source_name, .. }) => {
                    Some(source_name)
                }
                ExposeDecl::Dictionary(ExposeDictionaryDecl { source_name, .. }) => {
                    Some(source_name)
                }
            },
            ComponentCapability::Offer(offer) => match offer {
                OfferDecl::Protocol(OfferProtocolDecl { source_name, .. }) => Some(source_name),
                OfferDecl::Directory(OfferDirectoryDecl { source_name, .. }) => Some(source_name),
                OfferDecl::Runner(OfferRunnerDecl { source_name, .. }) => Some(source_name),
                OfferDecl::Storage(OfferStorageDecl { source_name, .. }) => Some(source_name),
                OfferDecl::Resolver(OfferResolverDecl { source_name, .. }) => Some(source_name),
                OfferDecl::Service(OfferServiceDecl { source_name, .. }) => Some(source_name),
                OfferDecl::Config(OfferConfigurationDecl { source_name, .. }) => Some(source_name),
                OfferDecl::EventStream(OfferEventStreamDecl { source_name, .. }) => {
                    Some(source_name)
                }
                OfferDecl::Dictionary(OfferDictionaryDecl { source_name, .. }) => Some(source_name),
            },
        }
    }

    pub fn source_capability_name(&self) -> Option<&Name> {
        match self {
            ComponentCapability::Offer(OfferDecl::Protocol(OfferProtocolDecl {
                source: OfferSource::Capability(name),
                ..
            })) => Some(name),
            ComponentCapability::Expose(ExposeDecl::Protocol(ExposeProtocolDecl {
                source: ExposeSource::Capability(name),
                ..
            })) => Some(name),
            ComponentCapability::Use_(UseDecl::Protocol(UseProtocolDecl {
                source: UseSource::Capability(name),
                ..
            })) => Some(name),
            _ => None,
        }
    }

    /// Returns the path or name of the capability as a string, useful for debugging.
    pub fn source_id(&self) -> String {
        self.source_name()
            .map(|p| format!("{}", p))
            .or_else(|| self.source_path().map(|p| format!("{}", p)))
            .unwrap_or_default()
    }
}

impl From<CapabilityDecl> for ComponentCapability {
    fn from(capability: CapabilityDecl) -> Self {
        match capability {
            CapabilityDecl::Service(c) => ComponentCapability::Service(c),
            CapabilityDecl::Protocol(c) => ComponentCapability::Protocol(c),
            CapabilityDecl::Directory(c) => ComponentCapability::Directory(c),
            CapabilityDecl::Storage(c) => ComponentCapability::Storage(c),
            CapabilityDecl::Runner(c) => ComponentCapability::Runner(c),
            CapabilityDecl::Resolver(c) => ComponentCapability::Resolver(c),
            CapabilityDecl::EventStream(c) => ComponentCapability::EventStream(c),
            CapabilityDecl::Dictionary(c) => ComponentCapability::Dictionary(c),
            CapabilityDecl::Config(c) => ComponentCapability::Config(c),
        }
    }
}

impl fmt::Display for ComponentCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} '{}' from component", self.type_name(), self.source_id())
    }
}

#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(tag = "type", rename_all = "snake_case")
)]
#[derive(FidlDecl, Clone, Debug, PartialEq, Eq)]
#[fidl_decl(fidl_union = "finternal::EnvironmentCapability")]
pub enum EnvironmentCapability {
    Runner(EnvironmentCapabilityData),
    Resolver(EnvironmentCapabilityData),
    Debug(EnvironmentCapabilityData),
}

impl EnvironmentCapability {
    pub fn registration_source(&self) -> &RegistrationSource {
        match self {
            Self::Runner(EnvironmentCapabilityData { source, .. })
            | Self::Resolver(EnvironmentCapabilityData { source, .. })
            | Self::Debug(EnvironmentCapabilityData { source, .. }) => &source,
        }
    }
}

#[cfg_attr(feature = "serde", derive(Deserialize, Serialize))]
#[derive(FidlDecl, Clone, Debug, PartialEq, Eq)]
#[fidl_decl(fidl_table = "finternal::EnvironmentSource")]
pub struct EnvironmentCapabilityData {
    source_name: Name,
    source: RegistrationSource,
}

/// Describes a capability provided by component manager that is an aggregation
/// of multiple instances of a capability.
#[cfg_attr(
    feature = "serde",
    derive(Deserialize, Serialize),
    serde(tag = "type", rename_all = "snake_case")
)]
#[derive(FidlDecl, Debug, Clone, PartialEq, Eq, Hash)]
#[fidl_decl(fidl_union = "finternal::AggregateCapability")]
pub enum AggregateCapability {
    Service(Name),
}

impl AggregateCapability {
    /// Returns true if the AggregateCapability can be available in a component's namespace.
    pub fn can_be_in_namespace(&self) -> bool {
        matches!(self, AggregateCapability::Service(_))
    }

    /// Returns a name for the capability type.
    pub fn type_name(&self) -> CapabilityTypeName {
        match self {
            AggregateCapability::Service(_) => CapabilityTypeName::Service,
        }
    }

    pub fn source_name(&self) -> &Name {
        match self {
            AggregateCapability::Service(name) => &name,
        }
    }
}

impl fmt::Display for AggregateCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "aggregate {} '{}'", self.type_name(), self.source_name())
    }
}

impl From<ServiceDecl> for AggregateCapability {
    fn from(service: ServiceDecl) -> Self {
        Self::Service(service.name)
    }
}

/// The list of declarations for capabilities from component manager's namespace.
pub type NamespaceCapabilities = Vec<CapabilityDecl>;

/// The list of declarations for capabilities offered by component manager as built-in capabilities.
pub type BuiltinCapabilities = Vec<CapabilityDecl>;

#[cfg(test)]
mod tests {
    use super::*;
    use cm_rust::StorageDirectorySource;

    #[test]
    fn capability_type_name() {
        let storage_capability = ComponentCapability::Storage(StorageDecl {
            name: "foo".parse().unwrap(),
            source: StorageDirectorySource::Parent,
            backing_dir: "bar".parse().unwrap(),
            subdir: Default::default(),
            storage_id: fdecl::StorageId::StaticInstanceIdOrMoniker,
        });
        assert_eq!(storage_capability.type_name(), CapabilityTypeName::Storage);
    }
}
