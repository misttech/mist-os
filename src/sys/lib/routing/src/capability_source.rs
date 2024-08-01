// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::bedrock::dict_ext::DictExt;
use crate::component_instance::{
    ComponentInstanceInterface, WeakComponentInstanceInterface, WeakExtendedInstanceInterface,
};
use crate::error::RoutingError;
use async_trait::async_trait;
use cm_rust::{
    CapabilityDecl, CapabilityTypeName, ConfigurationDecl, DictionaryDecl, DirectoryDecl,
    EventStreamDecl, ExposeConfigurationDecl, ExposeDecl, ExposeDictionaryDecl,
    ExposeDirectoryDecl, ExposeProtocolDecl, ExposeResolverDecl, ExposeRunnerDecl,
    ExposeServiceDecl, ExposeSource, FidlIntoNative, NameMapping, NativeIntoFidl,
    OfferConfigurationDecl, OfferDecl, OfferDictionaryDecl, OfferDirectoryDecl,
    OfferEventStreamDecl, OfferProtocolDecl, OfferResolverDecl, OfferRunnerDecl, OfferServiceDecl,
    OfferSource, OfferStorageDecl, ProtocolDecl, RegistrationSource, ResolverDecl, RunnerDecl,
    ServiceDecl, StorageDecl, UseDecl, UseDirectoryDecl, UseProtocolDecl, UseServiceDecl,
    UseSource, UseStorageDecl,
};
use cm_types::{Name, Path};
use derivative::Derivative;
use fidl::{persist, unpersist};
use fidl_fuchsia_component_decl as fdecl;
use from_enum::FromEnum;
use futures::future::BoxFuture;
use moniker::{ChildName, ExtendedMoniker, Moniker};
use sandbox::{Capability, Data, Dict};
use std::fmt;
use std::sync::Weak;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Invalid framework capability.")]
    InvalidFrameworkCapability {},
    #[error("Invalid builtin capability.")]
    InvalidBuiltinCapability {},
}

#[derive(Debug, Hash, PartialEq, Eq, Clone)]
pub enum AggregateMember {
    Child(ChildName),
    Collection(Name),
    Parent,
    Self_,
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

/// Describes the source of a capability, as determined by `find_capability_source`
#[derive(Debug, Derivative)]
#[derivative(Clone(bound = ""), PartialEq)]
pub enum CapabilitySource<C: ComponentInstanceInterface + 'static> {
    /// This capability originates from the component instance for the given Realm.
    /// point.
    Component { capability: ComponentCapability, moniker: Moniker },
    /// This capability originates from "framework". It's implemented by component manager and is
    /// scoped to the realm of the source.
    Framework { capability: InternalCapability, moniker: Moniker },
    /// This capability originates from the parent of the root component, and is built in to
    /// component manager. `top_instance` is the instance at the top of the tree, i.e.  the
    /// instance representing component manager.
    Builtin { capability: InternalCapability },
    /// This capability originates from the parent of the root component, and is offered from
    /// component manager's namespace. `top_instance` is the instance at the top of the tree, i.e.
    /// the instance representing component manager.
    Namespace {
        capability: ComponentCapability,
        #[derivative(PartialEq = "ignore")]
        top_instance: Weak<C::TopInstance>,
    },
    /// This capability is provided by the framework based on some other capability.
    Capability {
        source_capability: ComponentCapability,
        component: WeakComponentInstanceInterface<C>,
    },
    /// This capability is an aggregate of capabilities over a set of collections and static
    /// children. The instance names in the aggregate service will be anonymized.
    AnonymizedAggregate {
        capability: AggregateCapability,
        component: WeakComponentInstanceInterface<C>,
        #[derivative(PartialEq = "ignore")]
        aggregate_capability_provider: Box<dyn AnonymizedAggregateCapabilityProvider<C>>,
        members: Vec<AggregateMember>,
    },
    /// This capability is an aggregate of capabilities over a set of children with filters
    /// The instances in the aggregate service are the union of these filters.
    FilteredAggregate {
        capability: AggregateCapability,
        #[derivative(PartialEq = "ignore")]
        capability_provider: Box<dyn FilteredAggregateCapabilityProvider<C>>,
        component: WeakComponentInstanceInterface<C>,
    },
    /// This capability originates from "environment". It's implemented by a component instance.
    Environment { capability: ComponentCapability, component: WeakComponentInstanceInterface<C> },
    /// This capability originates from "void". This is only a valid origination for optional
    /// capabilities.
    Void { capability: InternalCapability, component: WeakComponentInstanceInterface<C> },
}

impl<C: ComponentInstanceInterface> CapabilitySource<C> {
    /// Returns whether the given CapabilitySourceInterface can be available in a component's
    /// namespace.
    pub fn can_be_in_namespace(&self) -> bool {
        match self {
            Self::Component { capability, .. } => capability.can_be_in_namespace(),
            Self::Framework { capability, .. } => capability.can_be_in_namespace(),
            Self::Builtin { capability, .. } => capability.can_be_in_namespace(),
            Self::Namespace { capability, .. } => capability.can_be_in_namespace(),
            Self::Capability { .. } => true,
            Self::AnonymizedAggregate { capability, .. } => capability.can_be_in_namespace(),
            Self::FilteredAggregate { capability, .. } => capability.can_be_in_namespace(),
            Self::Environment { capability, .. } => capability.can_be_in_namespace(),
            Self::Void { capability, .. } => capability.can_be_in_namespace(),
        }
    }

    pub fn source_name(&self) -> Option<&Name> {
        match self {
            Self::Component { capability, .. } => capability.source_name(),
            Self::Framework { capability, .. } => Some(capability.source_name()),
            Self::Builtin { capability, .. } => Some(capability.source_name()),
            Self::Namespace { capability, .. } => capability.source_name(),
            Self::Capability { .. } => None,
            Self::AnonymizedAggregate { capability, .. } => Some(capability.source_name()),
            Self::FilteredAggregate { capability, .. } => Some(capability.source_name()),
            Self::Environment { capability, .. } => capability.source_name(),
            Self::Void { capability, .. } => Some(capability.source_name()),
        }
    }

    pub fn type_name(&self) -> CapabilityTypeName {
        match self {
            Self::Component { capability, .. } => capability.type_name(),
            Self::Framework { capability, .. } => capability.type_name(),
            Self::Builtin { capability, .. } => capability.type_name(),
            Self::Namespace { capability, .. } => capability.type_name(),
            Self::Capability { source_capability, .. } => source_capability.type_name(),
            Self::AnonymizedAggregate { capability, .. } => capability.type_name(),
            Self::FilteredAggregate { capability, .. } => capability.type_name(),
            Self::Environment { capability, .. } => capability.type_name(),
            Self::Void { capability, .. } => capability.type_name(),
        }
    }

    pub fn source_moniker(&self) -> ExtendedMoniker {
        match self {
            Self::Component { moniker, .. } | Self::Framework { moniker, .. } => {
                ExtendedMoniker::ComponentInstance(moniker.clone())
            }
            Self::Capability { component, .. }
            | Self::AnonymizedAggregate { component, .. }
            | Self::FilteredAggregate { component, .. }
            | Self::Void { component, .. }
            | Self::Environment { component, .. } => {
                ExtendedMoniker::ComponentInstance(component.moniker.clone())
            }
            Self::Builtin { .. } | Self::Namespace { .. } => ExtendedMoniker::ComponentManager,
        }
    }
}

impl<C: ComponentInstanceInterface> fmt::Display for CapabilitySource<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Self::Component { capability, moniker } => {
                    format!("{} '{}'", capability, moniker)
                }
                Self::Framework { capability, .. } => capability.to_string(),
                Self::Builtin { capability, .. } => capability.to_string(),
                Self::Namespace { capability, .. } => capability.to_string(),
                Self::FilteredAggregate { capability, .. } => capability.to_string(),
                Self::Capability { source_capability, .. } => format!("{}", source_capability),
                Self::AnonymizedAggregate { capability, members, component, .. } => {
                    format!(
                        "{} from component '{}' aggregated from {}",
                        capability,
                        &component.moniker,
                        members.iter().map(|s| format!("{s}")).collect::<Vec<_>>().join(","),
                    )
                }
                Self::Environment { capability, .. } => capability.to_string(),
                Self::Void { capability, .. } => capability.to_string(),
            }
        )
    }
}

const AGGREGATE_CAPABILITY_KEY_STR: &'static str = "aggregate_capability_key";
const BUILTIN_STR: &'static str = "builtin";
const CAPABILITY_SOURCE_KEY_STR: &'static str = "capability_source_key";
const CAPABILITY_STR: &'static str = "capability";
const COMPONENT_CAPABILITY_KEY_STR: &'static str = "component_capability_key";
const COMPONENT_STR: &'static str = "component";
const CONFIG_STR: &'static str = "config";
const DEBUG_STR: &'static str = "debug";
const DICTIONARY_STR: &'static str = "dictionary";
const DIRECTORY_STR: &'static str = "directory";
const ENVIRONMENT_CAPABILITY_VARIANT_STR: &'static str = "environment_capability_variant";
const ENVIRONMENT_STR: &'static str = "environment";
const EVENT_STREAM_STR: &'static str = "event_stream";
const EXPOSE_STR: &'static str = "expose";
const FRAMEWORK_STR: &'static str = "framework";
const INTERNAL_CAPABILITY_KEY_STR: &'static str = "internal_capability_key";
const MONIKER_STR: &'static str = "moniker";
const NAMESPACE_STR: &'static str = "namespace";
const OFFER_STR: &'static str = "offer";
const PROTOCOL_STR: &'static str = "protocol";
const RESOLVER_STR: &'static str = "resolver";
const RUNNER_STR: &'static str = "runner";
const SERVICE_STR: &'static str = "service";
const SOURCE_NAME_STR: &'static str = "source_name";
const SOURCE_STR: &'static str = "source";
const STORAGE_STR: &'static str = "storage";
const USE_STR: &'static str = "use";
const VALUE_STR: &'static str = "value";
const VOID_STR: &'static str = "void";

impl<C: ComponentInstanceInterface + 'static> TryFrom<CapabilitySource<C>> for Capability {
    type Error = fidl::Error;

    fn try_from(capability_source: CapabilitySource<C>) -> Result<Self, Self::Error> {
        Ok(Capability::Dictionary(capability_source.try_into()?))
    }
}

impl<C: ComponentInstanceInterface + 'static> TryFrom<Capability> for CapabilitySource<C> {
    type Error = fidl::Error;

    fn try_from(capability: Capability) -> Result<Self, Self::Error> {
        let Capability::Dictionary(dictionary) = capability else {
            return Err(fidl::Error::InvalidEnumValue);
        };
        dictionary.try_into()
    }
}

impl<C: ComponentInstanceInterface + 'static> TryFrom<CapabilitySource<C>> for Dict {
    type Error = fidl::Error;

    fn try_from(capability_source: CapabilitySource<C>) -> Result<Self, Self::Error> {
        fn insert_key(dict: &Dict, key: &str) {
            dict.insert_capability(
                &Name::new(CAPABILITY_SOURCE_KEY_STR).unwrap(),
                Data::String(key.to_string()).into(),
            )
            .unwrap();
        }
        fn insert_capability_dict(
            dict: &Dict,
            try_into_dict: impl TryInto<Dict, Error = fidl::Error>,
        ) -> Result<(), fidl::Error> {
            dict.insert_capability(
                &Name::new(CAPABILITY_STR).unwrap(),
                Capability::Dictionary(try_into_dict.try_into()?),
            )
            .unwrap();
            Ok(())
        }
        fn insert_component_token<C: ComponentInstanceInterface + 'static>(
            dict: &Dict,
            component: WeakComponentInstanceInterface<C>,
        ) {
            dict.insert_capability(
                &Name::new(COMPONENT_STR).unwrap(),
                Capability::Instance(component.clone().into()),
            )
            .unwrap();
        }
        fn insert_top_instance_token<C: ComponentInstanceInterface + 'static>(
            dict: &Dict,
            top_instance: Weak<C::TopInstance>,
        ) {
            dict.insert_capability(
                &Name::new(COMPONENT_STR).unwrap(),
                Capability::Instance(
                    WeakExtendedInstanceInterface::<C>::AboveRoot(top_instance).into(),
                ),
            )
            .unwrap();
        }
        fn insert_moniker(dict: &Dict, moniker: Moniker) {
            dict.insert_capability(
                &Name::new(MONIKER_STR).unwrap(),
                Data::String(moniker.to_string()).into(),
            )
            .unwrap();
        }

        let output = Dict::new();
        match capability_source {
            CapabilitySource::Component { capability, moniker } => {
                insert_key(&output, COMPONENT_STR);
                insert_capability_dict(&output, capability)?;
                insert_moniker(&output, moniker)
            }
            CapabilitySource::Framework { capability, moniker } => {
                insert_key(&output, FRAMEWORK_STR);
                insert_capability_dict(&output, capability)?;
                insert_moniker(&output, moniker)
            }
            CapabilitySource::Builtin { capability } => {
                insert_key(&output, BUILTIN_STR);
                insert_capability_dict(&output, capability)?;
            }
            CapabilitySource::Namespace { capability, top_instance } => {
                insert_key(&output, NAMESPACE_STR);
                insert_capability_dict(&output, capability)?;
                insert_top_instance_token::<C>(&output, top_instance);
            }
            CapabilitySource::Capability { source_capability, component } => {
                insert_key(&output, CAPABILITY_STR);
                insert_capability_dict(&output, source_capability)?;
                insert_component_token(&output, component);
            }
            CapabilitySource::Environment { capability, component } => {
                insert_key(&output, ENVIRONMENT_STR);
                insert_capability_dict(&output, capability)?;
                insert_component_token(&output, component);
            }
            CapabilitySource::Void { capability, component } => {
                insert_key(&output, VOID_STR);
                insert_capability_dict(&output, capability)?;
                insert_component_token(&output, component);
            }
            // The following two are only relevant for service capabilities, which are currently
            // unsupported in the bedrock layer of routing.
            CapabilitySource::AnonymizedAggregate { .. } => unimplemented!(),
            CapabilitySource::FilteredAggregate { .. } => unimplemented!(),
        }
        Ok(output)
    }
}

impl<C: ComponentInstanceInterface + 'static> TryFrom<Dict> for CapabilitySource<C> {
    type Error = fidl::Error;

    fn try_from(dict: Dict) -> Result<Self, Self::Error> {
        fn get_capability_dict(dict: &Dict) -> Result<Dict, fidl::Error> {
            let data = dict
                .get(&Name::new(CAPABILITY_STR).unwrap())
                .map_err(|_| fidl::Error::InvalidEnumValue)?;
            let Some(Capability::Dictionary(capability_dict)) = data else {
                return Err(fidl::Error::InvalidEnumValue);
            };
            Ok(capability_dict)
        }
        fn get_weak_component<C: ComponentInstanceInterface + 'static>(
            dict: &Dict,
        ) -> Result<WeakComponentInstanceInterface<C>, fidl::Error> {
            let component = dict
                .get(&Name::new(COMPONENT_STR).unwrap())
                .map_err(|_| fidl::Error::InvalidEnumValue)?;
            let Some(Capability::Instance(weak_instance_token)) = component else {
                return Err(fidl::Error::InvalidEnumValue);
            };
            Ok(weak_instance_token
                .clone()
                .try_into()
                .expect("unexpected type in weak component token"))
        }
        fn get_top_instance<C: ComponentInstanceInterface + 'static>(
            dict: &Dict,
        ) -> Result<Weak<C::TopInstance>, fidl::Error> {
            let component = dict
                .get(&Name::new(COMPONENT_STR).unwrap())
                .map_err(|_| fidl::Error::InvalidEnumValue)?;
            let Some(Capability::Instance(weak_instance_token)) = component else {
                return Err(fidl::Error::InvalidEnumValue);
            };
            let weak_extended: WeakExtendedInstanceInterface<C> = weak_instance_token
                .clone()
                .try_into()
                .expect("unexpected type in weak component token");
            match weak_extended {
                WeakExtendedInstanceInterface::Component(_) => Err(fidl::Error::InvalidEnumValue),
                WeakExtendedInstanceInterface::AboveRoot(top_instance) => Ok(top_instance),
            }
        }
        fn get_moniker(dict: &Dict) -> Result<Moniker, fidl::Error> {
            let data = dict
                .get(&Name::new(MONIKER_STR).unwrap())
                .map_err(|_| fidl::Error::InvalidEnumValue)?;
            let Some(Capability::Data(Data::String(moniker_str))) = data else {
                return Err(fidl::Error::InvalidEnumValue);
            };
            moniker_str.as_str().try_into().map_err(|_| fidl::Error::InvalidEnumValue)
        }

        let key = dict
            .get(&Name::new(CAPABILITY_SOURCE_KEY_STR).unwrap())
            .map_err(|_| fidl::Error::InvalidEnumValue)?;
        let Some(Capability::Data(Data::String(key))) = key else {
            return Err(fidl::Error::InvalidEnumValue);
        };
        Ok(match key.as_str() {
            COMPONENT_STR => CapabilitySource::Component {
                capability: get_capability_dict(&dict)?.try_into()?,
                moniker: get_moniker(&dict)?,
            },
            FRAMEWORK_STR => CapabilitySource::Framework {
                capability: get_capability_dict(&dict)?.try_into()?,
                moniker: get_moniker(&dict)?,
            },
            BUILTIN_STR => {
                CapabilitySource::Builtin { capability: get_capability_dict(&dict)?.try_into()? }
            }
            NAMESPACE_STR => CapabilitySource::Namespace {
                capability: get_capability_dict(&dict)?.try_into()?,
                top_instance: get_top_instance::<C>(&dict)?,
            },
            CAPABILITY_STR => CapabilitySource::Capability {
                source_capability: get_capability_dict(&dict)?.try_into()?,
                component: get_weak_component(&dict)?,
            },
            ENVIRONMENT_STR => CapabilitySource::Environment {
                capability: get_capability_dict(&dict)?.try_into()?,
                component: get_weak_component(&dict)?,
            },
            VOID_STR => CapabilitySource::Void {
                capability: get_capability_dict(&dict)?.try_into()?,
                component: get_weak_component(&dict)?,
            },
            _ => return Err(fidl::Error::InvalidEnumValue),
        })
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

/// A provider of a capability from an aggregation of one or more collections and static children.
/// The instance names in the aggregate will be anonymized.
///
/// This trait type-erases the capability type, so it can be handled and hosted generically.
#[async_trait]
pub trait AnonymizedAggregateCapabilityProvider<C: ComponentInstanceInterface>:
    Send + Sync
{
    /// Lists the instances of the capability.
    ///
    /// The instance is an opaque identifier that is only meaningful for a subsequent
    /// call to `route_instance`.
    async fn list_instances(&self) -> Result<Vec<AggregateInstance>, RoutingError>;

    /// Route the given `instance` of the capability to its source.
    async fn route_instance(
        &self,
        instance: &AggregateInstance,
    ) -> Result<CapabilitySource<C>, RoutingError>;

    /// Trait-object compatible clone.
    fn clone_boxed(&self) -> Box<dyn AnonymizedAggregateCapabilityProvider<C>>;
}

impl<C: ComponentInstanceInterface> Clone for Box<dyn AnonymizedAggregateCapabilityProvider<C>> {
    fn clone(&self) -> Self {
        self.clone_boxed()
    }
}

impl<C> fmt::Debug for Box<dyn AnonymizedAggregateCapabilityProvider<C>> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Box<dyn AnonymizedAggregateCapabilityProvider>").finish()
    }
}

/// The return value of the routing future returned by
/// `FilteredAggregateCapabilityProvider::route_instances`, which contains information about the
/// source of the route.
#[derive(Debug)]
pub struct FilteredAggregateCapabilityRouteData<C>
where
    C: ComponentInstanceInterface + 'static,
{
    /// The source of the capability.
    pub capability_source: CapabilitySource<C>,
    /// The filter to apply to service instances, as defined by
    /// [`fuchsia.component.decl/OfferService.renamed_instances`](https://fuchsia.dev/reference/fidl/fuchsia.component.decl#OfferService).
    pub instance_filter: Vec<NameMapping>,
}

/// A provider of a capability from an aggregation of zero or more offered instances of a
/// capability, with filters.
///
/// This trait type-erases the capability type, so it can be handled and hosted generically.
pub trait FilteredAggregateCapabilityProvider<C: ComponentInstanceInterface>: Send + Sync {
    /// Return a list of futures to route every instance in the aggregate to its source. Each
    /// result is paired with the list of instances to include in the source.
    fn route_instances(
        &self,
    ) -> Vec<BoxFuture<'_, Result<FilteredAggregateCapabilityRouteData<C>, RoutingError>>>;

    /// Trait-object compatible clone.
    fn clone_boxed(&self) -> Box<dyn FilteredAggregateCapabilityProvider<C>>;
}

impl<C: ComponentInstanceInterface> Clone for Box<dyn FilteredAggregateCapabilityProvider<C>> {
    fn clone(&self) -> Self {
        self.clone_boxed()
    }
}

impl<C> fmt::Debug for Box<dyn FilteredAggregateCapabilityProvider<C>> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Box<dyn FilteredAggregateCapabilityProvider>").finish()
    }
}

/// Describes a capability provided by the component manager which could be a framework capability
/// scoped to a realm, a built-in global capability, or a capability from component manager's own
/// namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
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

impl TryFrom<InternalCapability> for Dict {
    type Error = fidl::Error;

    fn try_from(capability: InternalCapability) -> Result<Self, Self::Error> {
        let (key, value) = match capability {
            InternalCapability::Service(name) => (SERVICE_STR, name),
            InternalCapability::Protocol(name) => (PROTOCOL_STR, name),
            InternalCapability::Directory(name) => (DIRECTORY_STR, name),
            InternalCapability::Runner(name) => (RUNNER_STR, name),
            InternalCapability::Config(name) => (CONFIG_STR, name),
            InternalCapability::EventStream(name) => (EVENT_STREAM_STR, name),
            InternalCapability::Resolver(name) => (RESOLVER_STR, name),
            InternalCapability::Storage(name) => (STORAGE_STR, name),
            InternalCapability::Dictionary(name) => (DICTIONARY_STR, name),
        };
        let output = Dict::new();
        output
            .insert_capability(
                &Name::new(INTERNAL_CAPABILITY_KEY_STR).unwrap(),
                Data::String(key.to_string()).into(),
            )
            .unwrap();
        output
            .insert_capability(
                &Name::new(VALUE_STR).unwrap(),
                Data::String(value.as_str().to_string()).into(),
            )
            .unwrap();
        Ok(output)
    }
}

impl TryFrom<Dict> for InternalCapability {
    type Error = fidl::Error;

    fn try_from(dict: Dict) -> Result<Self, Self::Error> {
        let key = dict
            .get(&Name::new(INTERNAL_CAPABILITY_KEY_STR).unwrap())
            .map_err(|_| fidl::Error::InvalidEnumValue)?;
        let value =
            dict.get(&Name::new(VALUE_STR).unwrap()).map_err(|_| fidl::Error::InvalidEnumValue)?;
        let Some(Capability::Data(Data::String(key))) = key else {
            return Err(fidl::Error::InvalidEnumValue);
        };
        let Some(Capability::Data(Data::String(value))) = value else {
            return Err(fidl::Error::InvalidEnumValue);
        };
        let name = Name::new(value).unwrap();
        Ok(match key.as_str() {
            SERVICE_STR => InternalCapability::Service(name),
            PROTOCOL_STR => InternalCapability::Protocol(name),
            DIRECTORY_STR => InternalCapability::Directory(name),
            RUNNER_STR => InternalCapability::Runner(name),
            CONFIG_STR => InternalCapability::Config(name),
            EVENT_STREAM_STR => InternalCapability::EventStream(name),
            RESOLVER_STR => InternalCapability::Resolver(name),
            STORAGE_STR => InternalCapability::Storage(name),
            DICTIONARY_STR => InternalCapability::Dictionary(name),
            _ => panic!("unknown internal capability variant"),
        })
    }
}

/// A capability being routed from a component.
#[derive(FromEnum, Clone, Debug, PartialEq, Eq)]
pub enum ComponentCapability {
    Use(UseDecl),
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
            ComponentCapability::Use(use_) => {
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
            ComponentCapability::Use(use_) => use_.into(),
            ComponentCapability::Environment(env) => match env {
                EnvironmentCapability::Runner { .. } => CapabilityTypeName::Runner,
                EnvironmentCapability::Resolver { .. } => CapabilityTypeName::Resolver,
                EnvironmentCapability::Debug { .. } => CapabilityTypeName::Protocol,
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
            ComponentCapability::Use(use_) => match use_ {
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
                EnvironmentCapability::Runner { source_name, .. } => Some(source_name),
                EnvironmentCapability::Resolver { source_name, .. } => Some(source_name),
                EnvironmentCapability::Debug { source_name, .. } => Some(source_name),
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
            ComponentCapability::Use(UseDecl::Protocol(UseProtocolDecl {
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

impl TryFrom<ComponentCapability> for Dict {
    type Error = fidl::Error;

    fn try_from(capability: ComponentCapability) -> Result<Self, Self::Error> {
        let (key, value) = match capability {
            ComponentCapability::Use(decl) => {
                (USE_STR, Data::Bytes(persist(&decl.native_into_fidl())?).into())
            }
            ComponentCapability::Environment(env_cap) => {
                (ENVIRONMENT_STR, Capability::Dictionary(env_cap.try_into()?))
            }
            ComponentCapability::Expose(decl) => {
                (EXPOSE_STR, Data::Bytes(persist(&decl.native_into_fidl())?).into())
            }
            ComponentCapability::Offer(decl) => {
                (OFFER_STR, Data::Bytes(persist(&decl.native_into_fidl())?).into())
            }
            ComponentCapability::Protocol(decl) => {
                (PROTOCOL_STR, Data::Bytes(persist(&decl.native_into_fidl())?).into())
            }
            ComponentCapability::Directory(decl) => {
                (DIRECTORY_STR, Data::Bytes(persist(&decl.native_into_fidl())?).into())
            }
            ComponentCapability::Storage(decl) => {
                (STORAGE_STR, Data::Bytes(persist(&decl.native_into_fidl())?).into())
            }
            ComponentCapability::Runner(decl) => {
                (RUNNER_STR, Data::Bytes(persist(&decl.native_into_fidl())?).into())
            }
            ComponentCapability::Resolver(decl) => {
                (RESOLVER_STR, Data::Bytes(persist(&decl.native_into_fidl())?).into())
            }
            ComponentCapability::Service(decl) => {
                (SERVICE_STR, Data::Bytes(persist(&decl.native_into_fidl())?).into())
            }
            ComponentCapability::EventStream(decl) => {
                (EVENT_STREAM_STR, Data::Bytes(persist(&decl.native_into_fidl())?).into())
            }
            ComponentCapability::Dictionary(decl) => {
                (DICTIONARY_STR, Data::Bytes(persist(&decl.native_into_fidl())?).into())
            }
            ComponentCapability::Config(decl) => {
                (CONFIG_STR, Data::Bytes(persist(&decl.native_into_fidl())?).into())
            }
        };
        let output = Dict::new();
        output
            .insert_capability(
                &Name::new(COMPONENT_CAPABILITY_KEY_STR).unwrap(),
                Data::String(key.to_string()).into(),
            )
            .unwrap();
        output.insert_capability(&Name::new(VALUE_STR).unwrap(), value).unwrap();
        Ok(output)
    }
}

impl TryFrom<Dict> for ComponentCapability {
    type Error = fidl::Error;

    fn try_from(dict: Dict) -> Result<Self, Self::Error> {
        let key = dict
            .get(&Name::new(COMPONENT_CAPABILITY_KEY_STR).unwrap())
            .map_err(|_| fidl::Error::InvalidEnumValue)?;
        let value =
            dict.get(&Name::new(VALUE_STR).unwrap()).map_err(|_| fidl::Error::InvalidEnumValue)?;
        let Some(Capability::Data(Data::String(key))) = key else {
            return Err(fidl::Error::InvalidEnumValue);
        };
        if let Some(Capability::Dictionary(dict)) = value {
            if key.as_str() != ENVIRONMENT_STR {
                return Err(fidl::Error::InvalidEnumValue);
            }
            return Ok(ComponentCapability::Environment(dict.try_into()?));
        }
        let Some(Capability::Data(Data::Bytes(value))) = value else {
            return Err(fidl::Error::InvalidEnumValue);
        };
        Ok(match key.as_str() {
            USE_STR => {
                ComponentCapability::Use(unpersist::<fdecl::Use>(&value)?.fidl_into_native())
            }
            EXPOSE_STR => {
                ComponentCapability::Expose(unpersist::<fdecl::Expose>(&value)?.fidl_into_native())
            }
            OFFER_STR => {
                ComponentCapability::Offer(unpersist::<fdecl::Offer>(&value)?.fidl_into_native())
            }
            PROTOCOL_STR => ComponentCapability::Protocol(
                unpersist::<fdecl::Protocol>(&value)?.fidl_into_native(),
            ),
            DIRECTORY_STR => ComponentCapability::Directory(
                unpersist::<fdecl::Directory>(&value)?.fidl_into_native(),
            ),
            STORAGE_STR => ComponentCapability::Storage(
                unpersist::<fdecl::Storage>(&value)?.fidl_into_native(),
            ),
            RUNNER_STR => {
                ComponentCapability::Runner(unpersist::<fdecl::Runner>(&value)?.fidl_into_native())
            }
            RESOLVER_STR => ComponentCapability::Resolver(
                unpersist::<fdecl::Resolver>(&value)?.fidl_into_native(),
            ),
            SERVICE_STR => ComponentCapability::Service(
                unpersist::<fdecl::Service>(&value)?.fidl_into_native(),
            ),
            EVENT_STREAM_STR => ComponentCapability::EventStream(
                unpersist::<fdecl::EventStream>(&value)?.fidl_into_native(),
            ),
            DICTIONARY_STR => ComponentCapability::Dictionary(
                unpersist::<fdecl::Dictionary>(&value)?.fidl_into_native(),
            ),
            CONFIG_STR => ComponentCapability::Config(
                unpersist::<fdecl::Configuration>(&value)?.fidl_into_native(),
            ),
            _ => panic!("unknown component capability variant"),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvironmentCapability {
    Runner { source_name: Name, source: RegistrationSource },
    Resolver { source_name: Name, source: RegistrationSource },
    Debug { source_name: Name, source: RegistrationSource },
}

impl EnvironmentCapability {
    pub fn registration_source(&self) -> &RegistrationSource {
        match self {
            Self::Runner { source, .. }
            | Self::Resolver { source, .. }
            | Self::Debug { source, .. } => &source,
        }
    }
}

impl TryFrom<EnvironmentCapability> for Dict {
    type Error = fidl::Error;

    fn try_from(capability: EnvironmentCapability) -> Result<Self, Self::Error> {
        let (variant, source_name, source) = match capability {
            EnvironmentCapability::Runner { source_name, source } => {
                (RUNNER_STR, source_name, source)
            }
            EnvironmentCapability::Resolver { source_name, source } => {
                (RESOLVER_STR, source_name, source)
            }
            EnvironmentCapability::Debug { source_name, source } => {
                (DEBUG_STR, source_name, source)
            }
        };
        let output = Dict::new();
        output
            .insert_capability(
                &Name::new(ENVIRONMENT_CAPABILITY_VARIANT_STR).unwrap(),
                Data::String(variant.to_string()).into(),
            )
            .unwrap();
        output
            .insert_capability(
                &Name::new(SOURCE_NAME_STR).unwrap(),
                Data::String(source_name.as_str().to_string()).into(),
            )
            .unwrap();
        output
            .insert_capability(
                &Name::new(SOURCE_STR).unwrap(),
                Data::Bytes(persist(&source.native_into_fidl())?).into(),
            )
            .unwrap();
        Ok(output)
    }
}

impl TryFrom<Dict> for EnvironmentCapability {
    type Error = fidl::Error;

    fn try_from(dict: Dict) -> Result<Self, Self::Error> {
        let variant = dict
            .get(&Name::new(ENVIRONMENT_CAPABILITY_VARIANT_STR).unwrap())
            .map_err(|_| fidl::Error::InvalidEnumValue)?;
        let source_name = dict
            .get(&Name::new(SOURCE_NAME_STR).unwrap())
            .map_err(|_| fidl::Error::InvalidEnumValue)?;
        let source =
            dict.get(&Name::new(SOURCE_STR).unwrap()).map_err(|_| fidl::Error::InvalidEnumValue)?;

        let Some(Capability::Data(Data::String(variant))) = variant else {
            return Err(fidl::Error::InvalidEnumValue);
        };
        let Some(Capability::Data(Data::String(source_name))) = source_name else {
            return Err(fidl::Error::InvalidEnumValue);
        };
        let Some(Capability::Data(Data::Bytes(source))) = source else {
            return Err(fidl::Error::InvalidEnumValue);
        };
        let source_name = Name::new(source_name).map_err(|_e| fidl::Error::InvalidEnumValue)?;
        let source: RegistrationSource = unpersist::<fdecl::Ref>(&source)?.fidl_into_native();
        Ok(match variant.as_str() {
            RUNNER_STR => EnvironmentCapability::Runner { source_name, source },
            RESOLVER_STR => EnvironmentCapability::Resolver { source_name, source },
            DEBUG_STR => EnvironmentCapability::Debug { source_name, source },
            _ => panic!("unknown environment capability variant"),
        })
    }
}

/// Describes a capability provided by component manager that is an aggregation
/// of multiple instances of a capability.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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

impl TryFrom<AggregateCapability> for Dict {
    type Error = fidl::Error;

    fn try_from(capability: AggregateCapability) -> Result<Self, Self::Error> {
        let (key, value) = match capability {
            AggregateCapability::Service(name) => (SERVICE_STR, name),
        };
        let output = Dict::new();
        output
            .insert_capability(
                &Name::new(AGGREGATE_CAPABILITY_KEY_STR).unwrap(),
                Data::String(key.to_string()).into(),
            )
            .unwrap();
        output
            .insert_capability(
                &Name::new(VALUE_STR).unwrap(),
                Data::String(value.as_str().to_string()).into(),
            )
            .unwrap();
        Ok(output)
    }
}

impl TryFrom<Dict> for AggregateCapability {
    type Error = fidl::Error;

    fn try_from(dict: Dict) -> Result<Self, Self::Error> {
        let key = dict
            .get(&Name::new(AGGREGATE_CAPABILITY_KEY_STR).unwrap())
            .map_err(|_| fidl::Error::InvalidEnumValue)?;
        let value =
            dict.get(&Name::new(VALUE_STR).unwrap()).map_err(|_| fidl::Error::InvalidEnumValue)?;
        let Some(Capability::Data(Data::String(key))) = key else {
            return Err(fidl::Error::InvalidEnumValue);
        };
        let Some(Capability::Data(Data::String(value))) = value else {
            return Err(fidl::Error::InvalidEnumValue);
        };
        let name = Name::new(value).unwrap();
        Ok(match key.as_str() {
            SERVICE_STR => AggregateCapability::Service(name),
            _ => panic!("unknown aggregate capability variant"),
        })
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
