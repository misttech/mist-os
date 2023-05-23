// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A library of common utilities used by `cmc` and related tools.

pub mod error;
pub mod one_or_many;

#[allow(unused)] // A test-only macro is defined outside of a test builds.
pub mod translate;

use {
    crate::error::Error,
    cml_macro::{CheckedVec, OneOrMany, Reference},
    fidl_fuchsia_io as fio,
    indexmap::IndexMap,
    json5format::{FormatOptions, PathOption},
    lazy_static::lazy_static,
    maplit::{hashmap, hashset},
    paste::paste,
    reference_doc::ReferenceDoc,
    serde::{de, Deserialize, Serialize},
    serde_json::{Map, Value},
    std::{
        cmp,
        collections::{BTreeMap, HashMap, HashSet},
        fmt,
        hash::Hash,
        num::NonZeroU32,
        path,
        str::FromStr,
    },
};

pub use cm_types::{
    AllowedOffers, Availability, DependencyType, Durability, Name, OnTerminate, ParseError, Path,
    RelativePath, StartupMode, StorageId, Url,
};

pub use crate::{one_or_many::OneOrMany, translate::compile};

lazy_static! {
    static ref DEFAULT_EVENT_STREAM_NAME: Name = "EventStream".parse().unwrap();
}

/// A name/identity of a capability exposed/offered to another component.
///
/// Exposed or offered capabilities have an identifier whose format
/// depends on the capability type. For directories and services this is
/// a path, while for storage this is a storage name. Paths and storage
/// names, however, are in different conceptual namespaces, and can't
/// collide with each other.
///
/// This enum allows such names to be specified disambiguating what
/// namespace they are in.
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum CapabilityId {
    Service(Name),
    Protocol(Name),
    Directory(Name),
    // A service in a `use` declaration has a target path in the component's namespace.
    UsedService(Path),
    // A protocol in a `use` declaration has a target path in the component's namespace.
    UsedProtocol(Path),
    // A directory in a `use` declaration has a target path in the component's namespace.
    UsedDirectory(Path),
    // A storage in a `use` declaration has a target path in the component's namespace.
    UsedStorage(Path),
    // An event stream in a `use` declaration has a target path in the component's namespace.
    UsedEventStream(Path),
    Storage(Name),
    Runner(Name),
    Resolver(Name),
    EventStream(Name),
}

/// Generates a `Vec<Name>` -> `Vec<CapabilityId>` conversion function.
macro_rules! capability_ids_from_names {
    ($name:ident, $variant:expr) => {
        fn $name(names: Vec<Name>) -> Vec<Self> {
            names.into_iter().map(|n| $variant(n)).collect()
        }
    };
}

/// Generates a `Vec<Path>` -> `Vec<CapabilityId>` conversion function.
macro_rules! capability_ids_from_paths {
    ($name:ident, $variant:expr) => {
        fn $name(paths: Vec<Path>) -> Vec<Self> {
            paths.into_iter().map(|p| $variant(p)).collect()
        }
    };
}

impl CapabilityId {
    /// Human readable description of this capability type.
    pub fn type_str(&self) -> &'static str {
        match self {
            CapabilityId::Service(_) => "service",
            CapabilityId::Protocol(_) => "protocol",
            CapabilityId::Directory(_) => "directory",
            CapabilityId::UsedService(_) => "service",
            CapabilityId::UsedProtocol(_) => "protocol",
            CapabilityId::UsedDirectory(_) => "directory",
            CapabilityId::UsedStorage(_) => "storage",
            CapabilityId::UsedEventStream(_) => "event_stream",
            CapabilityId::Storage(_) => "storage",
            CapabilityId::Runner(_) => "runner",
            CapabilityId::Resolver(_) => "resolver",
            CapabilityId::EventStream(_) => "event_stream",
        }
    }

    /// Return the directory containing the capability.
    pub fn get_dir_path(&self) -> Option<&path::Path> {
        match self {
            CapabilityId::UsedService(p) => path::Path::new(p.as_str()).parent(),
            CapabilityId::UsedProtocol(p) => path::Path::new(p.as_str()).parent(),
            CapabilityId::UsedDirectory(p) => Some(path::Path::new(p.as_str())),
            CapabilityId::UsedStorage(p) => Some(path::Path::new(p.as_str())),
            CapabilityId::UsedEventStream(p) => path::Path::new(p.as_str()).parent(),
            _ => None,
        }
    }

    /// Given a Use clause, return the set of target identifiers.
    ///
    /// When only one capability identifier is specified, the target identifier name is derived
    /// using the "path" clause. If a "path" clause is not specified, the target identifier is the
    /// same name as the source.
    ///
    /// When multiple capability identifiers are specified, the target names are the same as the
    /// source names.
    pub fn from_use(use_: &Use) -> Result<Vec<CapabilityId>, Error> {
        // TODO: Validate that exactly one of these is set.
        let alias = use_.path.as_ref();
        if let Some(n) = use_.service() {
            return Ok(Self::used_services_from(Self::get_one_or_many_svc_paths(
                n,
                alias,
                use_.capability_type(),
            )?));
        } else if let Some(n) = use_.protocol() {
            return Ok(Self::used_protocols_from(Self::get_one_or_many_svc_paths(
                n,
                alias,
                use_.capability_type(),
            )?));
        } else if let Some(_) = use_.directory.as_ref() {
            if use_.path.is_none() {
                return Err(Error::validate("\"path\" should be present for `use directory`."));
            }
            return Ok(vec![CapabilityId::UsedDirectory(use_.path.as_ref().unwrap().clone())]);
        } else if let Some(_) = use_.storage.as_ref() {
            if use_.path.is_none() {
                return Err(Error::validate("\"path\" should be present for `use storage`."));
            }
            return Ok(vec![CapabilityId::UsedStorage(use_.path.as_ref().unwrap().clone())]);
        } else if let Some(_) = use_.event_stream() {
            if let Some(path) = use_.path() {
                return Ok(vec![CapabilityId::UsedEventStream(path.clone())]);
            }
            return Ok(vec![CapabilityId::UsedEventStream(Path::new(
                "/svc/fuchsia.component.EventStream".to_string(),
            )?)]);
        }
        // Unsupported capability type.
        let supported_keywords = use_
            .supported()
            .into_iter()
            .map(|k| format!("\"{}\"", k))
            .collect::<Vec<_>>()
            .join(", ");
        Err(Error::validate(format!(
            "`{}` declaration is missing a capability keyword, one of: {}",
            use_.decl_type(),
            supported_keywords,
        )))
    }

    pub fn from_capability(capability: &Capability) -> Result<Vec<CapabilityId>, Error> {
        // TODO: Validate that exactly one of these is set.
        if let Some(n) = capability.service() {
            if n.is_many() && capability.path.is_some() {
                return Err(Error::validate(
                    "\"path\" can only be specified when one `service` is supplied.",
                ));
            }
            return Ok(Self::services_from(Self::get_one_or_many_names(
                n,
                None,
                capability.capability_type(),
            )?));
        } else if let Some(n) = capability.protocol() {
            if n.is_many() && capability.path.is_some() {
                return Err(Error::validate(
                    "\"path\" can only be specified when one `protocol` is supplied.",
                ));
            }
            return Ok(Self::protocols_from(Self::get_one_or_many_names(
                n,
                None,
                capability.capability_type(),
            )?));
        } else if let Some(n) = capability.directory() {
            return Ok(Self::directories_from(Self::get_one_or_many_names(
                n,
                None,
                capability.capability_type(),
            )?));
        } else if let Some(n) = capability.storage() {
            if capability.storage_id.is_none() {
                return Err(Error::validate(
                    "Storage declaration is missing \"storage_id\", but is required.",
                ));
            }
            return Ok(Self::storages_from(Self::get_one_or_many_names(
                n,
                None,
                capability.capability_type(),
            )?));
        } else if let Some(n) = capability.runner() {
            return Ok(Self::runners_from(Self::get_one_or_many_names(
                n,
                None,
                capability.capability_type(),
            )?));
        } else if let Some(n) = capability.resolver() {
            return Ok(Self::resolvers_from(Self::get_one_or_many_names(
                n,
                None,
                capability.capability_type(),
            )?));
        } else if let Some(n) = capability.event_stream() {
            return Ok(Self::event_streams_from(Self::get_one_or_many_names(
                n,
                None,
                capability.capability_type(),
            )?));
        }

        // Unsupported capability type.
        let supported_keywords = capability
            .supported()
            .into_iter()
            .map(|k| format!("\"{}\"", k))
            .collect::<Vec<_>>()
            .join(", ");
        Err(Error::validate(format!(
            "`{}` declaration is missing a capability keyword, one of: {}",
            capability.decl_type(),
            supported_keywords,
        )))
    }

    /// Given an Offer or Expose clause, return the set of target identifiers.
    ///
    /// When only one capability identifier is specified, the target identifier name is derived
    /// using the "as" clause. If an "as" clause is not specified, the target identifier is the
    /// same name as the source.
    ///
    /// When multiple capability identifiers are specified, the target names are the same as the
    /// source names.
    pub fn from_offer_expose<T>(clause: &T) -> Result<Vec<CapabilityId>, Error>
    where
        T: CapabilityClause + AsClause + FilterClause + fmt::Debug,
    {
        // TODO: Validate that exactly one of these is set.
        let alias = clause.r#as();
        if let Some(n) = clause.service() {
            return Ok(Self::services_from(Self::get_one_or_many_names(
                n,
                alias,
                clause.capability_type(),
            )?));
        } else if let Some(n) = clause.protocol() {
            return Ok(Self::protocols_from(Self::get_one_or_many_names(
                n,
                alias,
                clause.capability_type(),
            )?));
        } else if let Some(n) = clause.directory() {
            return Ok(Self::directories_from(Self::get_one_or_many_names(
                n,
                alias,
                clause.capability_type(),
            )?));
        } else if let Some(n) = clause.storage() {
            return Ok(Self::storages_from(Self::get_one_or_many_names(
                n,
                alias,
                clause.capability_type(),
            )?));
        } else if let Some(n) = clause.runner() {
            return Ok(Self::runners_from(Self::get_one_or_many_names(
                n,
                alias,
                clause.capability_type(),
            )?));
        } else if let Some(n) = clause.resolver() {
            return Ok(Self::resolvers_from(Self::get_one_or_many_names(
                n,
                alias,
                clause.capability_type(),
            )?));
        } else if let Some(event_stream) = clause.event_stream() {
            return Ok(Self::event_streams_from(Self::get_one_or_many_names(
                event_stream,
                alias,
                clause.capability_type(),
            )?));
        }

        // Unsupported capability type.
        let supported_keywords = clause
            .supported()
            .into_iter()
            .map(|k| format!("\"{}\"", k))
            .collect::<Vec<_>>()
            .join(", ");
        Err(Error::validate(format!(
            "`{}` declaration is missing a capability keyword, one of: {}",
            clause.decl_type(),
            supported_keywords,
        )))
    }

    /// Returns the target names as a `Vec`  from a declaration with `names` and `alias` as a `Vec`.
    fn get_one_or_many_names(
        names: OneOrMany<Name>,
        alias: Option<&Name>,
        capability_type: &str,
    ) -> Result<Vec<Name>, Error> {
        let names: Vec<Name> = names.into_iter().collect();
        if names.len() == 1 {
            Ok(vec![alias_or_name(alias, &names[0])])
        } else {
            if alias.is_some() {
                return Err(Error::validate(format!(
                    "\"as\" can only be specified when one `{}` is supplied.",
                    capability_type,
                )));
            }
            Ok(names)
        }
    }

    /// Returns the target paths as a `Vec` from a `use` declaration with `names` and `alias`.
    fn get_one_or_many_svc_paths(
        names: OneOrMany<Name>,
        alias: Option<&Path>,
        capability_type: &str,
    ) -> Result<Vec<Path>, Error> {
        let names: Vec<Name> = names.into_iter().collect();
        match (names.len(), alias) {
            (_, None) => {
                Ok(names.into_iter().map(|n| format!("/svc/{}", n).parse().unwrap()).collect())
            }
            (1, Some(alias)) => Ok(vec![alias.clone()]),
            (_, Some(_)) => {
                return Err(Error::validate(format!(
                    "\"path\" can only be specified when one `{}` is supplied.",
                    capability_type,
                )));
            }
        }
    }

    capability_ids_from_names!(services_from, CapabilityId::Service);
    capability_ids_from_names!(protocols_from, CapabilityId::Protocol);
    capability_ids_from_names!(directories_from, CapabilityId::Directory);
    capability_ids_from_names!(storages_from, CapabilityId::Storage);
    capability_ids_from_names!(runners_from, CapabilityId::Runner);
    capability_ids_from_names!(resolvers_from, CapabilityId::Resolver);
    capability_ids_from_names!(event_streams_from, CapabilityId::EventStream);
    capability_ids_from_paths!(used_services_from, CapabilityId::UsedService);
    capability_ids_from_paths!(used_protocols_from, CapabilityId::UsedProtocol);
}

impl fmt::Display for CapabilityId {
    /// Return the string ID of this clause.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            CapabilityId::Service(n)
            | CapabilityId::Storage(n)
            | CapabilityId::Runner(n)
            | CapabilityId::Resolver(n)
            | CapabilityId::EventStream(n) => n.as_str(),
            CapabilityId::UsedService(p)
            | CapabilityId::UsedProtocol(p)
            | CapabilityId::UsedDirectory(p)
            | CapabilityId::UsedStorage(p)
            | CapabilityId::UsedEventStream(p) => p.as_str(),
            CapabilityId::Protocol(p) | CapabilityId::Directory(p) => p.as_str(),
        };
        write!(f, "{}", s)
    }
}

/// A list of rights.
#[derive(CheckedVec, Debug, PartialEq, Clone)]
#[checked_vec(
    expected = "a nonempty array of rights, with unique elements",
    min_length = 1,
    unique_items = true
)]
pub struct Rights(pub Vec<Right>);

/// Generates deserializer for `OneOrMany<Name>`.
#[derive(OneOrMany, Debug, Clone)]
#[one_or_many(
    expected = "a name or nonempty array of names, with unique elements",
    inner_type = "Name",
    min_length = 1,
    unique_items = true
)]
pub struct OneOrManyNames;

/// Generates deserializer for `OneOrMany<Path>`.
#[derive(OneOrMany, Debug, Clone)]
#[one_or_many(
    expected = "a path or nonempty array of paths, with unique elements",
    inner_type = "Path",
    min_length = 1,
    unique_items = true
)]
pub struct OneOrManyPaths;

/// Generates deserializer for `OneOrMany<ExposeFromRef>`.
#[derive(OneOrMany, Debug, Clone)]
#[one_or_many(
    expected = "one or an array of \"framework\", \"self\", or \"#<child-name>\"",
    inner_type = "ExposeFromRef",
    min_length = 1,
    unique_items = true
)]
pub struct OneOrManyExposeFromRefs;

/// Generates deserializer for `OneOrMany<OfferToRef>`.
#[derive(OneOrMany, Debug, Clone)]
#[one_or_many(
    expected = "one or an array of \"#<child-name>\" or \"#<collection-name>\", with unique elements",
    inner_type = "OfferToRef",
    min_length = 1,
    unique_items = true
)]
pub struct OneOrManyOfferToRefs;

/// Generates deserializer for `OneOrMany<OfferFromRef>`.
#[derive(OneOrMany, Debug, Clone)]
#[one_or_many(
    expected = "one or an array of \"parent\", \"framework\", \"self\", \"#<child-name>\", or \"#<collection-name>\"",
    inner_type = "OfferFromRef",
    min_length = 1,
    unique_items = true
)]
pub struct OneOrManyOfferFromRefs;

/// Generates deserializer for `OneOrMany<UseFromRef>`.
#[derive(OneOrMany, Debug, Clone)]
#[one_or_many(
    expected = "one or an array of \"#<collection-name>\", or \"#<child-name>\"",
    inner_type = "EventScope",
    min_length = 1,
    unique_items = true
)]
pub struct OneOrManyEventScope;

/// The stop timeout configured in an environment.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct StopTimeoutMs(pub u32);

impl<'de> de::Deserialize<'de> for StopTimeoutMs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = StopTimeoutMs;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an unsigned 32-bit integer")
            }

            fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                if v < 0 || v > i64::from(u32::max_value()) {
                    return Err(E::invalid_value(
                        de::Unexpected::Signed(v),
                        &"an unsigned 32-bit integer",
                    ));
                }
                Ok(StopTimeoutMs(v as u32))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                self.visit_i64(value as i64)
            }
        }

        deserializer.deserialize_i64(Visitor)
    }
}

/// A relative reference to another object. This is a generic type that can encode any supported
/// reference subtype. For named references, it holds a reference to the name instead of the name
/// itself.
///
/// Objects of this type are usually derived from conversions of context-specific reference
/// types that `#[derive(Reference)]`. This type makes it easy to write helper functions that operate on
/// generic references.
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub enum AnyRef<'a> {
    /// A named reference. Parsed as `#name`.
    Named(&'a Name),
    /// A reference to the parent. Parsed as `parent`.
    Parent,
    /// A reference to the framework (component manager). Parsed as `framework`.
    Framework,

    /// A reference to the debug. Parsed as `debug`.
    Debug,
    /// A reference to this component. Parsed as `self`.
    Self_,
    /// An intentionally omitted reference.
    Void,
}

/// Format an `AnyRef` as a string.
impl fmt::Display for AnyRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Named(name) => write!(f, "#{}", name),
            Self::Parent => write!(f, "parent"),
            Self::Framework => write!(f, "framework"),
            Self::Debug => write!(f, "debug"),
            Self::Self_ => write!(f, "self"),
            Self::Void => write!(f, "void"),
        }
    }
}

/// A reference in a `use from`.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Reference)]
#[reference(
    expected = "\"parent\", \"framework\", \"debug\", \"self\", \"#<capability-name>\", \"#<child-name>\", or none"
)]
pub enum UseFromRef {
    /// A reference to the parent.
    Parent,
    /// A reference to the framework.
    Framework,
    /// A reference to debug.
    Debug,
    /// A reference to a child or a capability declared on self.
    ///
    /// A reference to a capability is only valid on use declarations for a protocol that
    /// references a storage capability declared in the same component, which will cause the
    /// framework to host a fuchsia.sys2.StorageAdmin protocol for the component.
    ///
    /// This cannot be used to directly access capabilities that a component itself declares.
    Named(Name),
    /// A reference to this component.
    Self_,
}

/// The scope of an event.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Reference)]
#[reference(expected = "\"#<collection-name>\", \"#<child-name>\", or none")]
pub enum EventScope {
    /// A reference to a child or a collection.
    Named(Name),
}

/// A reference in an `expose from`.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Reference)]
#[reference(expected = "\"framework\", \"self\", \"void\", or \"#<child-name>\"")]
pub enum ExposeFromRef {
    /// A reference to a child or collection.
    Named(Name),
    /// A reference to the framework.
    Framework,
    /// A reference to this component.
    Self_,
    /// An intentionally omitted source.
    Void,
}

/// A reference in an `expose to`.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Reference)]
#[reference(expected = "\"parent\", \"framework\", or none")]
pub enum ExposeToRef {
    /// A reference to the parent.
    Parent,
    /// A reference to the framework.
    Framework,
}

/// A reference in an `offer from`.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Reference)]
#[reference(expected = "\"parent\", \"framework\", \"self\", \"void\", or \"#<child-name>\"")]
pub enum OfferFromRef {
    /// A reference to a child or collection.
    Named(Name),
    /// A reference to the parent.
    Parent,
    /// A reference to the framework.
    Framework,
    /// A reference to this component.
    Self_,
    /// An intentionally omitted source.
    Void,
}

impl OfferFromRef {
    pub fn is_named(&self) -> bool {
        match self {
            OfferFromRef::Named(_) => true,
            _ => false,
        }
    }
}

/// A reference in an `offer to`.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Reference)]
#[reference(expected = "\"#<child-name>\" or \"#<collection-name>\"")]
pub enum OfferToRef {
    /// A reference to a child or collection.
    Named(Name),

    /// Syntax sugar that results in the offer decl applying to all children and collections
    All,
}

/// A reference in an `offer to`.
#[derive(Debug, Deserialize, PartialEq, Eq, Hash, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAvailability {
    Required,
    Unknown,
}

impl Default for SourceAvailability {
    fn default() -> Self {
        Self::Required
    }
}

/// A reference in an environment.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Reference)]
#[reference(expected = "\"#<environment-name>\"")]
pub enum EnvironmentRef {
    /// A reference to an environment defined in this component.
    Named(Name),
}

/// A reference in a `storage from`.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Reference)]
#[reference(expected = "\"parent\", \"self\", or \"#<child-name>\"")]
pub enum CapabilityFromRef {
    /// A reference to a child.
    Named(Name),
    /// A reference to the parent.
    Parent,
    /// A reference to this component.
    Self_,
}

/// A reference in an environment registration.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Reference)]
#[reference(expected = "\"parent\", \"self\", or \"#<child-name>\"")]
pub enum RegistrationRef {
    /// A reference to a child.
    Named(Name),
    /// A reference to the parent.
    Parent,
    /// A reference to this component.
    Self_,
}

/// A right or bundle of rights to apply to a directory.
#[derive(Deserialize, Clone, Debug, Eq, PartialEq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Right {
    // Individual
    Connect,
    Enumerate,
    Execute,
    GetAttributes,
    ModifyDirectory,
    ReadBytes,
    Traverse,
    UpdateAttributes,
    WriteBytes,

    // Aliass
    #[serde(rename = "r*")]
    ReadAlias,
    #[serde(rename = "w*")]
    WriteAlias,
    #[serde(rename = "x*")]
    ExecuteAlias,
    #[serde(rename = "rw*")]
    ReadWriteAlias,
    #[serde(rename = "rx*")]
    ReadExecuteAlias,
}

impl fmt::Display for Right {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Connect => "connect",
            Self::Enumerate => "enumerate",
            Self::Execute => "execute",
            Self::GetAttributes => "get_attributes",
            Self::ModifyDirectory => "modify_directory",
            Self::ReadBytes => "read_bytes",
            Self::Traverse => "traverse",
            Self::UpdateAttributes => "update_attributes",
            Self::WriteBytes => "write_bytes",
            Self::ReadAlias => "r*",
            Self::WriteAlias => "w*",
            Self::ExecuteAlias => "x*",
            Self::ReadWriteAlias => "rw*",
            Self::ReadExecuteAlias => "rx*",
        };
        write!(f, "{}", s)
    }
}

impl Right {
    /// Expands this right or bundle or rights into a list of `fio::Operations`.
    pub fn expand(&self) -> Vec<fio::Operations> {
        match self {
            Self::Connect => vec![fio::Operations::CONNECT],
            Self::Enumerate => vec![fio::Operations::ENUMERATE],
            Self::Execute => vec![fio::Operations::EXECUTE],
            Self::GetAttributes => vec![fio::Operations::GET_ATTRIBUTES],
            Self::ModifyDirectory => vec![fio::Operations::MODIFY_DIRECTORY],
            Self::ReadBytes => vec![fio::Operations::READ_BYTES],
            Self::Traverse => vec![fio::Operations::TRAVERSE],
            Self::UpdateAttributes => vec![fio::Operations::UPDATE_ATTRIBUTES],
            Self::WriteBytes => vec![fio::Operations::WRITE_BYTES],
            Self::ReadAlias => vec![
                fio::Operations::CONNECT,
                fio::Operations::ENUMERATE,
                fio::Operations::TRAVERSE,
                fio::Operations::READ_BYTES,
                fio::Operations::GET_ATTRIBUTES,
            ],
            Self::WriteAlias => vec![
                fio::Operations::CONNECT,
                fio::Operations::ENUMERATE,
                fio::Operations::TRAVERSE,
                fio::Operations::WRITE_BYTES,
                fio::Operations::MODIFY_DIRECTORY,
                fio::Operations::UPDATE_ATTRIBUTES,
            ],
            Self::ExecuteAlias => vec![
                fio::Operations::CONNECT,
                fio::Operations::ENUMERATE,
                fio::Operations::TRAVERSE,
                fio::Operations::EXECUTE,
            ],
            Self::ReadWriteAlias => vec![
                fio::Operations::CONNECT,
                fio::Operations::ENUMERATE,
                fio::Operations::TRAVERSE,
                fio::Operations::READ_BYTES,
                fio::Operations::WRITE_BYTES,
                fio::Operations::MODIFY_DIRECTORY,
                fio::Operations::GET_ATTRIBUTES,
                fio::Operations::UPDATE_ATTRIBUTES,
            ],
            Self::ReadExecuteAlias => vec![
                fio::Operations::CONNECT,
                fio::Operations::ENUMERATE,
                fio::Operations::TRAVERSE,
                fio::Operations::READ_BYTES,
                fio::Operations::GET_ATTRIBUTES,
                fio::Operations::EXECUTE,
            ],
        }
    }
}

/// # Component manifest (`.cml`) reference
///
/// A `.cml` file contains a single json5 object literal with the keys below.
///
/// Where string values are expected, a list of valid values is generally documented.
/// The following string value types are reused and must follow specific rules.
///
/// ## String types
///
/// ### Names {#names}
///
/// Both capabilities and a component's children are named. A name string must consist of one or
/// more of the following characters: `a-z`, `0-9`, `_`, `.`, `-`.
///
/// ### References {#references}
///
/// A reference string takes the form of `#<name>`, where `<name>` refers to the name of a child:
///
/// - A [static child instance][doc-static-children] whose name is
///     `<name>`, or
/// - A [collection][doc-collections] whose name is `<name>`.
///
/// [doc-static-children]: /docs/concepts/components/v2/realms.md#static-children
/// [doc-collections]: /docs/concepts/components/v2/realms.md#collections
/// [doc-protocol]: /docs/concepts/components/v2/capabilities/protocol.md
/// [doc-directory]: /docs/concepts/components/v2/capabilities/directory.md
/// [doc-storage]: /docs/concepts/components/v2/capabilities/storage.md
/// [doc-resolvers]: /docs/concepts/components/v2/capabilities/resolvers.md
/// [doc-runners]: /docs/concepts/components/v2/capabilities/runners.md
/// [doc-event]: /docs/concepts/components/v2/capabilities/event.md
/// [doc-service]: /docs/concepts/components/v2/capabilities/service.md
/// [doc-directory-rights]: /docs/concepts/components/v2/capabilities/directory#directory-capability-rights
///
/// ## Top-level keys
#[derive(ReferenceDoc, Deserialize, Debug, Default, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    /// The optional `include` property describes zero or more other component manifest
    /// files to be merged into this component manifest. For example:
    ///
    /// ```json5
    /// include: [ "syslog/client.shard.cml" ]
    /// ```
    ///
    /// In the example given above, the component manifest is including contents from a
    /// manifest shard provided by the `syslog` library, thus ensuring that the
    /// component functions correctly at runtime if it attempts to write to `syslog`. By
    /// convention such files are called "manifest shards" and end with `.shard.cml`.
    ///
    /// Include paths prepended with `//` are relative to the source root of the Fuchsia
    /// checkout. However, include paths not prepended with `//`, as in the example
    /// above, are resolved from Fuchsia SDK libraries (`//sdk/lib`) that export
    /// component manifest shards.
    ///
    /// For reference, inside the Fuchsia checkout these two include paths are
    /// equivalent:
    ///
    /// * `syslog/client.shard.cml`
    /// * `//sdk/lib/syslog/client.shard.cml`
    ///
    /// You can review the outcome of merging any and all includes into a component
    /// manifest file by invoking the following command:
    ///
    /// Note: The `fx` command below is for developers working in a Fuchsia source
    /// checkout environment.
    ///
    /// ```sh
    /// fx cmc include {{ "<var>" }}cmx_file{{ "</var>" }} --includeroot $FUCHSIA_DIR --includepath $FUCHSIA_DIR/sdk/lib
    /// ```
    ///
    /// Includes can cope with duplicate [`use`], [`offer`], [`expose`], or [`capabilities`]
    /// declarations referencing the same capability, as long as the properties are the same. For
    /// example:
    ///
    /// ```json5
    /// // my_component.cml
    /// include: [ "syslog.client.shard.cml" ]
    /// use: [
    ///     {
    ///         protocol: [
    ///             "fuchsia.logger.LogSink",
    ///             "fuchsia.posix.socket.Provider",
    ///         ],
    ///     },
    /// ],
    ///
    /// // syslog.client.shard.cml
    /// use: [
    ///     { protocol: "fuchsia.logger.LogSink" },
    /// ],
    /// ```
    ///
    /// In this example, the contents of the merged file will be the same as my_component.cml --
    /// `fuchsia.logger.LogSink` is deduped.
    ///
    /// However, this would fail to compile:
    ///
    /// ```json5
    /// // my_component.cml
    /// include: [ "syslog.client.shard.cml" ]
    /// use: [
    ///     {
    ///         protocol: "fuchsia.logger.LogSink",
    ///         // properties for fuchsia.logger.LogSink don't match
    ///         from: "#archivist",
    ///     },
    /// ],
    ///
    /// // syslog.client.shard.cml
    /// use: [
    ///     { protocol: "fuchsia.logger.LogSink" },
    /// ],
    /// ```
    ///
    /// An exception to this constraint is the `availability` property. If two routing declarations
    /// are identical, and one availability is stronger than the other, the availability will be
    /// "promoted" to the stronger value (if `availability` is missing, it defaults to `required`).
    /// For example:
    ///
    /// ```json5
    /// // my_component.cml
    /// include: [ "syslog.client.shard.cml" ]
    /// use: [
    ///     {
    ///         protocol: [
    ///             "fuchsia.logger.LogSink",
    ///             "fuchsia.posix.socket.Provider",
    ///         ],
    ///         availability: "optional",
    ///     },
    /// ],
    ///
    /// // syslog.client.shard.cml
    /// use: [
    ///     {
    ///         protocol: "fuchsia.logger.LogSink"
    ///         availability: "required",  // This is the default
    ///     },
    /// ],
    /// ```
    ///
    /// Becomes:
    ///
    /// ```json5
    /// use: [
    ///     {
    ///         protocol: "fuchsia.posix.socket.Provider",
    ///         availability: "optional",
    ///     },
    ///     {
    ///         protocol: "fuchsia.logger.LogSink",
    ///         availability: "required",
    ///     },
    /// ],
    /// ```
    ///
    /// Includes are transitive, meaning that shards can have their own includes.
    ///
    /// Include paths can have diamond dependencies. For instance this is valid:
    /// A includes B, A includes C, B includes D, C includes D.
    /// In this case A will transitively include B, C, D.
    ///
    /// Include paths cannot have cycles. For instance this is invalid:
    /// A includes B, B includes A.
    /// A cycle such as the above will result in a compile-time error.
    ///
    /// [`use`]: struct.Document.html#use
    /// [`offer`]: struct.Document.html#offer
    /// [`expose`]: struct.Document.html#expose
    /// [`capabilities`]: struct.Document.html#capabilities
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,

    /// The `disable` section disables certain features in a particular CML that are
    /// otherwise enforced by cmc.
    #[reference_doc(recurse)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable: Option<Disable>,

    /// Components that are executable include a `program` section. The `program`
    /// section must set the `runner` property to select a [runner][doc-runners] to run
    /// the component. The format of the rest of the `program` section is determined by
    /// that particular runner.
    ///
    /// # ELF runners {#elf-runners}
    ///
    /// If the component uses the ELF runner, `program` must include the following
    /// properties, at a minimum:
    ///
    /// - `runner`: must be set to `"elf"`
    /// - `binary`: Package-relative path to the executable binary
    /// - `args` _(optional)_: List of arguments
    ///
    /// Example:
    ///
    /// ```json5
    /// program: {
    ///     runner: "elf",
    ///     binary: "bin/hippo",
    ///     args: [ "Hello", "hippos!" ],
    /// },
    /// ```
    ///
    /// For a complete list of properties, see: [ELF Runner](/docs/concepts/components/v2/elf_runner.md)
    ///
    /// # Other runners {#other-runners}
    ///
    /// If a component uses a custom runner, values inside the `program` stanza other
    /// than `runner` are specific to the runner. The runner receives the arguments as a
    /// dictionary of key and value pairs. Refer to the specific runner being used to
    /// determine what keys it expects to receive, and how it interprets them.
    ///
    /// [doc-runners]: /docs/concepts/components/v2/capabilities/runners.md
    #[reference_doc(json_type = "object")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub program: Option<Program>,

    /// The `children` section declares child component instances as described in
    /// [Child component instances][doc-children].
    ///
    /// [doc-children]: /docs/concepts/components/v2/realms.md#child-component-instances
    #[reference_doc(recurse)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<Child>>,

    /// The `collections` section declares collections as described in
    /// [Component collections][doc-collections].
    #[reference_doc(recurse)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collections: Option<Vec<Collection>>,

    /// The `environments` section declares environments as described in
    /// [Environments][doc-environments].
    ///
    /// [doc-environments]: /docs/concepts/components/v2/environments.md
    #[reference_doc(recurse)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environments: Option<Vec<Environment>>,

    /// The `capabilities` section defines capabilities that are provided by this component.
    /// Capabilities that are [offered](#offer) or [exposed](#expose) from `self` must be declared
    /// here.
    ///
    /// One and only one of the capability type keys (`protocol`, `directory`, `service`, ...) is required.
    ///
    /// [glossary.outgoing directory]: /docs/glossary/README.md#outgoing-directory
    #[reference_doc(recurse)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<Vec<Capability>>,

    /// For executable components, declares capabilities that this
    /// component requires in its [namespace][glossary.namespace] at runtime.
    /// Capabilities are routed from the `parent` unless otherwise specified,
    /// and each capability must have a valid route through all components between
    /// this component and the capability's source.
    ///
    /// [fidl-environment-decl]: /reference/fidl/fuchsia.component.decl#Environment
    /// [glossary.namespace]: /docs/glossary/README.md#namespace
    #[reference_doc(recurse)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#use: Option<Vec<Use>>,

    /// Declares the capabilities that are made available to the parent component or to the
    /// framework. It is valid to `expose` from `self` or from a child component.
    ///
    /// One and only one of the capability type keys (`protocol`, `directory`, `service`, ...) is required.
    #[reference_doc(recurse)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expose: Option<Vec<Expose>>,

    /// Declares the capabilities that are made available to a [child component][doc-children]
    /// instance or a [child collection][doc-collections].
    #[reference_doc(recurse)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offer: Option<Vec<Offer>>,

    /// Contains metadata that components may interpret for their own purposes. The component
    /// framework enforces no schema for this section, but third parties may expect their facets to
    /// adhere to a particular schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub facets: Option<IndexMap<String, Value>>,

    /// The configuration schema as defined by a component. Each key represents a single field
    /// in the schema.
    ///
    /// Configuration fields are JSON objects and must define a `type` which can be one of the
    /// following strings:
    /// `bool`, `uint8`, `int8`, `uint16`, `int16`, `uint32`, `int32`, `uint64`, `int64`,
    /// `string`, `vector`
    ///
    /// Example:
    ///
    /// ```json5
    /// config: {
    ///     debug_mode: {
    ///         type: "bool"
    ///     },
    /// }
    /// ```
    ///
    /// Strings must define the `max_size` property as a non-zero integer.
    ///
    /// Example:
    ///
    /// ```json5
    /// config: {
    ///     verbosity: {
    ///         type: "string",
    ///         max_size: 20,
    ///     }
    /// }
    /// ```
    ///
    /// Vectors must set the `max_count` property as a non-zero integer. Vectors must also set the
    /// `element` property as a JSON object which describes the element being contained in the
    /// vector. Vectors can contain booleans, integers, and strings but cannot contain other
    /// vectors.
    ///
    /// Example:
    ///
    /// ```json5
    /// config: {
    ///     tags: {
    ///         type: "vector",
    ///         max_count: 20,
    ///         element: {
    ///             type: "string",
    ///             max_size: 50,
    ///         }
    ///     }
    /// }
    /// ```
    #[reference_doc(json_type = "object")]
    #[serde(skip_serializing_if = "Option::is_none")]
    // NB: Unlike other maps the order of these fields matters for the ABI of generated config
    // libraries. Rather than insertion order, we explicitly sort the fields here to dissuade
    // developers from taking a dependency on the source ordering in their manifest. In the future
    // this will hopefully make it easier to pursue layout size optimizations.
    pub config: Option<BTreeMap<ConfigKey, ConfigValueType>>,
}

/// Merges `$self.$field_name` into `$other.$field_name` according to the rules documented for
/// [`include`], where `$self` and `$other` are of type [`Document`] and $field_name is a
/// capability routing field of [`Document`]: `use`, `offer`, `expose`, or `capabilities`.
///
/// [`include`]: struct.Document.html#include
/// [`Document`]: struct.Document.html
macro_rules! merge_from_capability_field {
    ($self:ident, $other:ident, $field_name:ident) => {
        if let Some(all_ours) = $self.$field_name.as_mut() {
            if let Some(all_theirs) = $other.$field_name.take() {
                for mut theirs in all_theirs {
                    for ours in &mut *all_ours {
                        compute_diff(ours, &mut theirs);
                    }
                    all_ours.push(theirs);
                }
            }
            // Post-filter step: remove empty entries.
            all_ours.retain(|ours| {
                let mut nonempty = false;
                for list in [
                    ours.service(),
                    ours.protocol(),
                    ours.directory(),
                    ours.storage(),
                    ours.resolver(),
                    ours.runner(),
                    ours.event_stream(),
                ] {
                    nonempty |= list.is_some();
                }
                nonempty
            });
        } else if let Some(theirs) = $other.$field_name.take() {
            $self.$field_name.replace(theirs);
        }
    };
}

/// Merges `$self.$field_name` into `$other.$field_name` according to the rules documented for
/// [`include`], where `$self` and `$other` are of type [`Document`] and $field_name is _not_ a
/// capability routing field of [`Document`] (`use`, `offer`, `expose`, or `capabilities`).
///
/// [`include`]: struct.Document.html#include
/// [`Document`]: struct.Document.html
macro_rules! merge_from_other_field {
    ($self:ident, $other:ident, $field_name:ident) => {
        if let Some(ref mut ours) = $self.$field_name {
            if let Some(theirs) = $other.$field_name.take() {
                // Add their elements, ignoring dupes with ours
                for t in theirs {
                    if !ours.contains(&t) {
                        ours.push(t);
                    }
                }
            }
        } else if let Some(theirs) = $other.$field_name.take() {
            $self.$field_name.replace(theirs);
        }
    };
}

/// Subtracts the capabilities in `$ours` from `$theirs` if the type matches `$type_name` and the
/// declarations match. Stores the result in `$theirs`.
///
/// Inexact matches on `availability` are allowed if there is a partial order between them. The
/// stronger availability is chosen.
///
/// This is a macro to let us easily specialize on the $type_name.
macro_rules! compute_capability_diff {
    ($ours:ident, $theirs:ident, $type_name:ident) => {
        let our_field = $ours.$type_name();
        let their_field = $theirs.$type_name();
        match (our_field, their_field) {
            (_, None) | (None, _) => {} // One of the declarations is not for `type_name`.
            (Some(our_field), Some(their_field)) => {
                // Check if the non-capability fields match before proceeding.
                let mut ours_partial = $ours.clone();
                let mut theirs_partial = $theirs.clone();
                for e in [&mut ours_partial, &mut theirs_partial] {
                    paste! { e.[<set_ $type_name>](None) };
                    // Availability is allowed to differ (see merge algorithm below)
                    e.set_availability(None);
                }
                let avail_cmp = $ours
                    .availability()
                    .unwrap_or_default()
                    .partial_cmp(&$theirs.availability().unwrap_or_default());
                if ours_partial != theirs_partial || avail_cmp.is_none() {
                    // One of the following is true:
                    // - The fields other than `availability` do not match.
                    // - The fields other than `availability` do match, but `availability`
                    //   does not and is incompatible (no partial order).
                    // Either way, `ours` and `theirs` are not diffable.
                } else {
                    let avail_cmp = avail_cmp.unwrap();
                    let mut our_entries_to_remove = HashSet::new();
                    let mut their_entries_to_remove = HashSet::new();
                    for e in &their_field {
                        if !our_field.contains(e) {
                            // Not a duplicate, so keep.
                        } else {
                            match avail_cmp {
                                cmp::Ordering::Less => {
                                    // Their availability is stronger, meaning theirs should take
                                    // priority. Keep `e` in theirs, and remove it from ours.
                                    our_entries_to_remove.insert(e);
                                }
                                cmp::Ordering::Greater => {
                                    // Our availability is stronger, meaning ours should take
                                    // priority. Remove `e` from theirs.
                                    their_entries_to_remove.insert(e);
                                }
                                cmp::Ordering::Equal => {
                                    // The availabilities are equal, so `e` is a duplicate.
                                    their_entries_to_remove.insert(e);
                                }
                            }
                        }
                    }
                    fn update_entries(
                        decl: &mut impl CapabilityClause,
                        field: &OneOrMany<Name>,
                        to_remove: &HashSet<&Name>,
                    ) {
                        if to_remove.is_empty() {
                            return;
                        }
                        let mut new_entries: Vec<_> = field.iter().cloned().collect();
                        new_entries.retain(|e| !to_remove.contains(&e));
                        let new_entries = match new_entries.len() {
                            1 => Some(OneOrMany::One(new_entries.remove(0))),
                            0 => None,
                            _ => Some(OneOrMany::Many(new_entries)),
                        };
                        paste! { decl.[<set_ $type_name>](new_entries); }
                    }
                    update_entries($ours, &our_field, &our_entries_to_remove);
                    update_entries($theirs, &their_field, &their_entries_to_remove);
                }
            }
        }
    };
}

/// Subtracts the capabilities in `ours` from `theirs` if the declarations match in their type and
/// other fields, resulting in the removal of duplicates between `ours` and `theirs`. Stores the
/// result in `theirs`. Returns true if `theirs` is empty, which is a signal to the caller that it
/// can be discarded.
fn compute_diff<T>(ours: &mut T, theirs: &mut T)
where
    T: CapabilityClause,
{
    compute_capability_diff!(ours, theirs, service);
    compute_capability_diff!(ours, theirs, protocol);
    compute_capability_diff!(ours, theirs, directory);
    compute_capability_diff!(ours, theirs, storage);
    compute_capability_diff!(ours, theirs, resolver);
    compute_capability_diff!(ours, theirs, runner);
    compute_capability_diff!(ours, theirs, event_stream);
}

impl Document {
    pub fn merge_from(
        &mut self,
        other: &mut Document,
        include_path: &path::Path,
    ) -> Result<(), Error> {
        // Flatten the mergable fields that may contain a
        // list of capabilities in one clause.
        merge_from_capability_field!(self, other, r#use);
        merge_from_capability_field!(self, other, expose);
        merge_from_capability_field!(self, other, offer);
        merge_from_capability_field!(self, other, capabilities);
        merge_from_other_field!(self, other, include);
        merge_from_other_field!(self, other, children);
        merge_from_other_field!(self, other, collections);
        merge_from_other_field!(self, other, environments);
        self.merge_program(other, include_path)?;
        self.merge_facets(other, include_path)?;
        self.merge_config(other, include_path)?;

        Ok(())
    }

    fn merge_program(
        &mut self,
        other: &mut Document,
        include_path: &path::Path,
    ) -> Result<(), Error> {
        if let None = other.program {
            return Ok(());
        }
        if let None = self.program {
            self.program = Some(Program::default());
        }
        let my_program = self.program.as_mut().unwrap();
        let other_program = other.program.as_mut().unwrap();
        if let Some(other_runner) = other_program.runner.take() {
            my_program.runner = match &my_program.runner {
                Some(runner) if *runner != other_runner => {
                    return Err(Error::validate(format!(
                        "manifest include had a conflicting `program.runner`: {}",
                        include_path.display()
                    )))
                }
                _ => Some(other_runner),
            }
        }

        Self::merge_maps_with_options(
            &mut my_program.info,
            &other_program.info,
            "program",
            include_path,
            Some(vec!["environ"]),
        )
    }

    fn merge_maps<'s, Source, Dest>(
        self_map: &mut Dest,
        include_map: Source,
        outer_key: &str,
        include_path: &path::Path,
    ) -> Result<(), Error>
    where
        Source: IntoIterator<Item = (&'s String, &'s Value)>,
        Dest: ValueMap,
    {
        Self::merge_maps_with_options(self_map, include_map, outer_key, include_path, None)
    }

    /// If `allow_array_concatenation_keys` is None, all arrays present in both
    /// `self_map` and `include_map` will be concatenated in the result. If it
    /// is set to Some(vec), only those keys specified will allow concatenation,
    /// with any others returning an error.
    fn merge_maps_with_options<'s, Source, Dest>(
        self_map: &mut Dest,
        include_map: Source,
        outer_key: &str,
        include_path: &path::Path,
        allow_array_concatenation_keys: Option<Vec<&str>>,
    ) -> Result<(), Error>
    where
        Source: IntoIterator<Item = (&'s String, &'s Value)>,
        Dest: ValueMap,
    {
        for (key, value) in include_map {
            match self_map.get_mut(key) {
                None => {
                    // Key not present in self map, insert it from include map.
                    self_map.insert(key.clone(), value.clone());
                }
                // Self and include maps share the same key
                Some(Value::Object(self_nested_map)) => match value {
                    // The include value is an object and can be recursively merged
                    Value::Object(include_nested_map) => {
                        let combined_key = format!("{}.{}", outer_key, key);

                        // Recursively merge maps
                        Self::merge_maps(
                            self_nested_map,
                            include_nested_map,
                            &combined_key,
                            include_path,
                        )?;
                    }
                    _ => {
                        // Cannot merge object and non-object
                        return Err(Error::validate(format!(
                            "manifest include had a conflicting `{}.{}`: {}",
                            outer_key,
                            key,
                            include_path.display()
                        )));
                    }
                },
                Some(Value::Array(self_nested_vec)) => match value {
                    // The include value is an array and can be merged, unless
                    // `allow_array_concatenation_keys` is used and the key is not included.
                    Value::Array(include_nested_vec) => {
                        if let Some(allowed_keys) = &allow_array_concatenation_keys {
                            if !allowed_keys.contains(&key.as_str()) {
                                // This key wasn't present in `allow_array_concatenation_keys` and so
                                // merging is disallowed.
                                return Err(Error::validate(format!(
                                    "manifest include had a conflicting `{}.{}`: {}",
                                    outer_key,
                                    key,
                                    include_path.display()
                                )));
                            }
                        }
                        let mut new_values = include_nested_vec.clone();
                        self_nested_vec.append(&mut new_values);
                    }
                    _ => {
                        // Cannot merge array and non-array
                        return Err(Error::validate(format!(
                            "manifest include had a conflicting `{}.{}`: {}",
                            outer_key,
                            key,
                            include_path.display()
                        )));
                    }
                },
                _ => {
                    // Cannot merge object and non-object
                    return Err(Error::validate(format!(
                        "manifest include had a conflicting `{}.{}`: {}",
                        outer_key,
                        key,
                        include_path.display()
                    )));
                }
            }
        }
        Ok(())
    }

    fn merge_facets(
        &mut self,
        other: &mut Document,
        include_path: &path::Path,
    ) -> Result<(), Error> {
        if let None = other.facets {
            return Ok(());
        }
        if let None = self.facets {
            self.facets = Some(Default::default());
        }
        let my_facets = self.facets.as_mut().unwrap();
        let other_facets = other.facets.as_ref().unwrap();

        Self::merge_maps(my_facets, other_facets, "facets", include_path)
    }

    fn merge_config(
        &mut self,
        other: &mut Document,
        include_path: &path::Path,
    ) -> Result<(), Error> {
        if let Some(other_config) = other.config.as_mut() {
            if let Some(self_config) = self.config.as_mut() {
                for (key, field) in other_config {
                    match self_config.entry(key.clone()) {
                        std::collections::btree_map::Entry::Vacant(v) => {
                            v.insert(field.clone());
                        }
                        std::collections::btree_map::Entry::Occupied(o) => {
                            if o.get() != field {
                                let msg = format!(
                                    "Found conflicting entry for config key `{key}` in `{}`.",
                                    include_path.display()
                                );
                                return Err(Error::validate(&msg));
                            }
                        }
                    }
                }
            } else {
                self.config.replace(std::mem::take(other_config));
            }
        }
        Ok(())
    }

    pub fn includes(&self) -> Vec<String> {
        self.include.clone().unwrap_or_default()
    }

    pub fn all_children_names(&self) -> Vec<&Name> {
        if let Some(children) = self.children.as_ref() {
            children.iter().map(|c| &c.name).collect()
        } else {
            vec![]
        }
    }

    pub fn all_collection_names(&self) -> Vec<&Name> {
        if let Some(collections) = self.collections.as_ref() {
            collections.iter().map(|c| &c.name).collect()
        } else {
            vec![]
        }
    }

    pub fn all_storage_names(&self) -> Vec<&Name> {
        if let Some(capabilities) = self.capabilities.as_ref() {
            capabilities.iter().filter_map(|c| c.storage.as_ref()).collect()
        } else {
            vec![]
        }
    }

    pub fn all_storage_and_sources<'a>(&'a self) -> HashMap<&'a Name, &'a CapabilityFromRef> {
        if let Some(capabilities) = self.capabilities.as_ref() {
            capabilities
                .iter()
                .filter_map(|c| match (c.storage.as_ref(), c.from.as_ref()) {
                    (Some(s), Some(f)) => Some((s, f)),
                    _ => None,
                })
                .collect()
        } else {
            HashMap::new()
        }
    }

    pub fn all_service_names(&self) -> Vec<&Name> {
        self.capabilities
            .as_ref()
            .map(|c| {
                c.iter()
                    .filter_map(|c| c.service.as_ref())
                    .map(|p| p.to_vec().into_iter())
                    .flatten()
                    .collect()
            })
            .unwrap_or_else(|| vec![])
    }

    pub fn all_protocol_names(&self) -> Vec<&Name> {
        self.capabilities
            .as_ref()
            .map(|c| {
                c.iter()
                    .filter_map(|c| c.protocol.as_ref())
                    .map(|p| p.to_vec().into_iter())
                    .flatten()
                    .collect()
            })
            .unwrap_or_else(|| vec![])
    }

    pub fn all_directory_names(&self) -> Vec<&Name> {
        self.capabilities
            .as_ref()
            .map(|c| c.iter().filter_map(|c| c.directory.as_ref()).collect())
            .unwrap_or_else(|| vec![])
    }

    pub fn all_runner_names(&self) -> Vec<&Name> {
        self.capabilities
            .as_ref()
            .map(|c| c.iter().filter_map(|c| c.runner.as_ref()).collect())
            .unwrap_or_else(|| vec![])
    }

    pub fn all_resolver_names(&self) -> Vec<&Name> {
        self.capabilities
            .as_ref()
            .map(|c| c.iter().filter_map(|c| c.resolver.as_ref()).collect())
            .unwrap_or_else(|| vec![])
    }

    pub fn all_environment_names(&self) -> Vec<&Name> {
        self.environments
            .as_ref()
            .map(|c| c.iter().map(|s| &s.name).collect())
            .unwrap_or_else(|| vec![])
    }

    pub fn all_capability_names(&self) -> HashSet<Name> {
        self.capabilities
            .as_ref()
            .map(|c| {
                c.iter().fold(HashSet::new(), |mut acc, capability| {
                    acc.extend(capability.names());
                    acc
                })
            })
            .unwrap_or_default()
    }
}

/// Trait that allows us to merge `serde_json::Map`s into `indexmap::IndexMap`s and vice versa.
trait ValueMap {
    fn get_mut(&mut self, key: &str) -> Option<&mut Value>;
    fn insert(&mut self, key: String, val: Value);
}

impl ValueMap for Map<String, Value> {
    fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.get_mut(key)
    }

    fn insert(&mut self, key: String, val: Value) {
        self.insert(key, val);
    }
}

impl ValueMap for IndexMap<String, Value> {
    fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.get_mut(key)
    }

    fn insert(&mut self, key: String, val: Value) {
        self.insert(key, val);
    }
}

#[derive(Deserialize, Debug, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EnvironmentExtends {
    Realm,
    None,
}

/// Example:
///
/// ```json5
/// environments: [
///     {
///         name: "test-env",
///         extend: "realm",
///         runners: [
///             {
///                 runner: "gtest-runner",
///                 from: "#gtest",
///             },
///         ],
///         resolvers: [
///             {
///                 resolver: "full-resolver",
///                 from: "parent",
///                 scheme: "fuchsia-pkg",
///             },
///         ],
///     },
/// ],
/// ```
#[derive(Deserialize, Debug, PartialEq, ReferenceDoc, Serialize)]
#[serde(deny_unknown_fields)]
#[reference_doc(fields_as = "list", top_level_doc_after_fields)]
pub struct Environment {
    /// The name of the environment, which is a string of one or more of the
    /// following characters: `a-z`, `0-9`, `_`, `.`, `-`. The name identifies this
    /// environment when used in a [reference](#references).
    pub name: Name,

    /// How the environment should extend this realm's environment.
    /// - `realm`: Inherit all properties from this component's environment.
    /// - `none`: Start with an empty environment, do not inherit anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extends: Option<EnvironmentExtends>,

    /// The runners registered in the environment. An array of objects
    /// with the following properties:
    #[reference_doc(recurse)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runners: Option<Vec<RunnerRegistration>>,

    /// The resolvers registered in the environment. An array of
    /// objects with the following properties:
    #[reference_doc(recurse)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolvers: Option<Vec<ResolverRegistration>>,

    /// Debug protocols available to any component in this environment acquired
    /// through `use from debug`.
    #[reference_doc(recurse)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub debug: Option<Vec<DebugRegistration>>,

    /// The number of milliseconds to wait, after notifying a component in this environment that it
    /// should terminate, before forcibly killing it.
    #[serde(rename = "__stop_timeout_ms")]
    #[reference_doc(json_type = "number", rename = "__stop_timeout_ms")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_timeout_ms: Option<StopTimeoutMs>,
}

#[derive(Clone, Hash, Debug, PartialEq, PartialOrd, Eq, Ord, Serialize)]
pub struct ConfigKey(String);

impl ConfigKey {
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl std::fmt::Display for ConfigKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for ConfigKey {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, ParseError> {
        let length = s.len();
        if length == 0 {
            return Err(ParseError::Empty);
        }
        if length > 64 {
            return Err(ParseError::TooLong);
        }

        // identifiers must start with a letter
        let first_is_letter = s.chars().next().expect("non-empty string").is_ascii_lowercase();
        // can contain letters, numbers, and underscores
        let contains_invalid_chars =
            s.chars().any(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'));
        // cannot end with an underscore
        let last_is_underscore = s.chars().next_back().expect("non-empty string") == '_';

        if !first_is_letter || contains_invalid_chars || last_is_underscore {
            return Err(ParseError::InvalidValue);
        }

        Ok(Self(s.to_string()))
    }
}

impl<'de> de::Deserialize<'de> for ConfigKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = ConfigKey;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    "a non-empty string no more than 64 characters in length, which must \
                    start with a letter, can contain letters, numbers, and underscores, \
                    but cannot end with an underscore",
                )
            }

            fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                s.parse().map_err(|err| match err {
                    ParseError::InvalidValue => E::invalid_value(
                        de::Unexpected::Str(s),
                        &"a name which must start with a letter, can contain letters, \
                        numbers, and underscores, but cannot end with an underscore",
                    ),
                    ParseError::TooLong | ParseError::Empty => E::invalid_length(
                        s.len(),
                        &"a non-empty name no more than 64 characters in length",
                    ),
                    e => {
                        panic!("unexpected parse error: {:?}", e);
                    }
                })
            }
        }
        deserializer.deserialize_string(Visitor)
    }
}

#[derive(Clone, Deserialize, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum ConfigRuntimeSource {
    Parent,
}

#[derive(Clone, Deserialize, Debug, PartialEq, Serialize)]
#[serde(tag = "type", deny_unknown_fields, rename_all = "lowercase")]
pub enum ConfigValueType {
    Bool {
        mutability: Option<Vec<ConfigRuntimeSource>>,
    },
    Uint8 {
        mutability: Option<Vec<ConfigRuntimeSource>>,
    },
    Uint16 {
        mutability: Option<Vec<ConfigRuntimeSource>>,
    },
    Uint32 {
        mutability: Option<Vec<ConfigRuntimeSource>>,
    },
    Uint64 {
        mutability: Option<Vec<ConfigRuntimeSource>>,
    },
    Int8 {
        mutability: Option<Vec<ConfigRuntimeSource>>,
    },
    Int16 {
        mutability: Option<Vec<ConfigRuntimeSource>>,
    },
    Int32 {
        mutability: Option<Vec<ConfigRuntimeSource>>,
    },
    Int64 {
        mutability: Option<Vec<ConfigRuntimeSource>>,
    },
    String {
        max_size: NonZeroU32,
        mutability: Option<Vec<ConfigRuntimeSource>>,
    },
    Vector {
        max_count: NonZeroU32,
        element: ConfigNestedValueType,
        mutability: Option<Vec<ConfigRuntimeSource>>,
    },
}

impl ConfigValueType {
    /// Update the hasher by digesting the ConfigValueType enum value
    pub fn update_digest(&self, hasher: &mut impl sha2::Digest) {
        let val = match self {
            ConfigValueType::Bool { .. } => 0u8,
            ConfigValueType::Uint8 { .. } => 1u8,
            ConfigValueType::Uint16 { .. } => 2u8,
            ConfigValueType::Uint32 { .. } => 3u8,
            ConfigValueType::Uint64 { .. } => 4u8,
            ConfigValueType::Int8 { .. } => 5u8,
            ConfigValueType::Int16 { .. } => 6u8,
            ConfigValueType::Int32 { .. } => 7u8,
            ConfigValueType::Int64 { .. } => 8u8,
            ConfigValueType::String { max_size, .. } => {
                hasher.update(max_size.get().to_le_bytes());
                9u8
            }
            ConfigValueType::Vector { max_count, element, .. } => {
                hasher.update(max_count.get().to_le_bytes());
                element.update_digest(hasher);
                10u8
            }
        };
        hasher.update([val])
    }
}

#[derive(Clone, Deserialize, Debug, PartialEq, Serialize)]
#[serde(tag = "type", deny_unknown_fields, rename_all = "lowercase")]
pub enum ConfigNestedValueType {
    Bool {},
    Uint8 {},
    Uint16 {},
    Uint32 {},
    Uint64 {},
    Int8 {},
    Int16 {},
    Int32 {},
    Int64 {},
    String { max_size: NonZeroU32 },
}

impl ConfigNestedValueType {
    /// Update the hasher by digesting the ConfigVectorElementType enum value
    pub fn update_digest(&self, hasher: &mut impl sha2::Digest) {
        let val = match self {
            ConfigNestedValueType::Bool {} => 0u8,
            ConfigNestedValueType::Uint8 {} => 1u8,
            ConfigNestedValueType::Uint16 {} => 2u8,
            ConfigNestedValueType::Uint32 {} => 3u8,
            ConfigNestedValueType::Uint64 {} => 4u8,
            ConfigNestedValueType::Int8 {} => 5u8,
            ConfigNestedValueType::Int16 {} => 6u8,
            ConfigNestedValueType::Int32 {} => 7u8,
            ConfigNestedValueType::Int64 {} => 8u8,
            ConfigNestedValueType::String { max_size } => {
                hasher.update(max_size.get().to_le_bytes());
                9u8
            }
        };
        hasher.update([val])
    }
}

#[derive(Deserialize, Debug, PartialEq, ReferenceDoc, Serialize)]
#[serde(deny_unknown_fields)]
#[reference_doc(fields_as = "list")]
pub struct RunnerRegistration {
    /// The [name](#name) of a runner capability, whose source is specified in `from`.
    pub runner: Name,

    /// The source of the runner capability, one of:
    /// - `parent`: The component's parent.
    /// - `self`: This component.
    /// - `#<child-name>`: A [reference](#references) to a child component
    ///     instance.
    pub from: RegistrationRef,

    /// An explicit name for the runner as it will be known in
    /// this environment. If omitted, defaults to `runner`.
    pub r#as: Option<Name>,
}

#[derive(Deserialize, Debug, PartialEq, ReferenceDoc, Serialize)]
#[serde(deny_unknown_fields)]
#[reference_doc(fields_as = "list")]
pub struct ResolverRegistration {
    /// The [name](#name) of a resolver capability,
    /// whose source is specified in `from`.
    pub resolver: Name,

    /// The source of the resolver capability, one of:
    /// - `parent`: The component's parent.
    /// - `self`: This component.
    /// - `#<child-name>`: A [reference](#references) to a child component
    ///     instance.
    pub from: RegistrationRef,

    /// The URL scheme for which the resolver should handle
    /// resolution.
    pub scheme: cm_types::UrlScheme,
}

#[derive(Deserialize, Debug, PartialEq, Clone, ReferenceDoc, Serialize)]
#[serde(deny_unknown_fields)]
#[reference_doc(fields_as = "list")]
pub struct Capability {
    /// The [name](#name) for this service capability. Specifying `path` is valid
    /// only when this value is a string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<OneOrMany<Name>>,

    /// The [name](#name) for this protocol capability. Specifying `path` is valid
    /// only when this value is a string.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<OneOrMany<Name>>,

    /// The [name](#name) for this directory capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<Name>,

    /// The [name](#name) for this storage capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<Name>,

    /// The [name](#name) for this runner capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner: Option<Name>,

    /// The [name](#name) for this resolver capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolver: Option<Name>,

    /// The [name](#name) for this event_stream capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_stream: Option<OneOrMany<Name>>,

    /// The path within the [outgoing directory][glossary.outgoing directory] of the component's
    /// program to source the capability.
    ///
    /// For `protocol` and `service`, defaults to `/svc/${protocol}`, otherwise required.
    ///
    /// For `protocol`, the target of the path MUST be a channel, which tends to speak
    /// the protocol matching the name of this capability.
    ///
    /// For `service`, `directory`, the target of the path MUST be a directory.
    ///
    /// For `runner`, the target of the path MUST be a channel and MUST speak
    /// the protocol `fuchsia.component.runner.ComponentRunner`.
    ///
    /// For `resolver`, the target of the path MUST be a channel and MUST speak
    /// the protocol `fuchsia.component.resolution.Resolver`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<Path>,

    /// (`directory` only) The maximum [directory rights][doc-directory-rights] that may be set
    /// when using this directory.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(json_type = "array of string")]
    pub rights: Option<Rights>,

    /// (`storage` only) The source component of an existing directory capability backing this
    /// storage capability, one of:
    /// - `parent`: The component's parent.
    /// - `self`: This component.
    /// - `#<child-name>`: A [reference](#references) to a child component
    ///     instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<CapabilityFromRef>,

    /// (`storage` only) The [name](#name) of the directory capability backing the storage. The
    /// capability must be available from the component referenced in `from`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backing_dir: Option<Name>,

    /// (`storage` only) A subdirectory within `backing_dir` where per-component isolated storage
    /// directories are created
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdir: Option<RelativePath>,

    /// (`storage only`) The identifier used to isolated storage for a component, one of:
    /// - `static_instance_id`: The instance ID in the component ID index is used
    ///     as the key for a component's storage. Components which are not listed in
    ///     the component ID index will not be able to use this storage capability.
    /// - `static_instance_id_or_moniker`: If the component is listed in the
    ///     component ID index, the instance ID is used as the key for a component's
    ///     storage. Otherwise, the component's relative moniker from the storage
    ///     capability is used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_id: Option<StorageId>,
}

#[derive(Deserialize, Debug, Clone, PartialEq, ReferenceDoc, Serialize)]
#[serde(deny_unknown_fields)]
#[reference_doc(fields_as = "list")]
pub struct DebugRegistration {
    /// The name(s) of the protocol(s) to make available.
    pub protocol: Option<OneOrMany<Name>>,

    /// The source of the capability(s), one of:
    /// - `parent`: The component's parent.
    /// - `self`: This component.
    /// - `#<child-name>`: A [reference](#references) to a child component
    ///     instance.
    pub from: OfferFromRef,

    /// If specified, the name that the capability in `protocol` should be made
    /// available as to clients. Disallowed if `protocol` is an array.
    pub r#as: Option<Name>,
}

#[derive(Debug, PartialEq, Default, Serialize)]
pub struct Program {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner: Option<Name>,
    #[serde(flatten)]
    pub info: IndexMap<String, Value>,
}

impl<'de> de::Deserialize<'de> for Program {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        struct Visitor;

        const EXPECTED_PROGRAM: &'static str =
            "a JSON object that includes a `runner` string property";
        const EXPECTED_RUNNER: &'static str =
            "a non-empty `runner` string property no more than 100 characters in length \
            that consists of [A-Za-z0-9_.-] and starts with [A-Za-z0-9_]";

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Program;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(EXPECTED_PROGRAM)
            }

            fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
            where
                A: de::MapAccess<'de>,
            {
                let mut info = IndexMap::new();
                let mut runner = None;
                while let Some(e) = map.next_entry::<String, Value>()? {
                    let (k, v) = e;
                    if &k == "runner" {
                        if let Value::String(s) = v {
                            runner = Some(s);
                        } else {
                            return Err(de::Error::invalid_value(
                                de::Unexpected::Map,
                                &EXPECTED_RUNNER,
                            ));
                        }
                    } else {
                        info.insert(k, v);
                    }
                }
                let runner = runner
                    .map(|r| {
                        Name::try_new(r.clone()).map_err(|e| match e {
                            ParseError::InvalidValue => de::Error::invalid_value(
                                serde::de::Unexpected::Str(&r),
                                &EXPECTED_RUNNER,
                            ),
                            ParseError::TooLong | ParseError::Empty => {
                                de::Error::invalid_length(r.len(), &EXPECTED_RUNNER)
                            }
                            _ => {
                                panic!("unexpected parse error: {:?}", e);
                            }
                        })
                    })
                    .transpose()?;
                Ok(Program { runner, info })
            }
        }

        deserializer.deserialize_map(Visitor)
    }
}

/// Example:
///
/// ```json5
/// use: [
///     {
///         protocol: [
///             "fuchsia.ui.scenic.Scenic",
///             "fuchsia.accessibility.Manager",
///         ]
///     },
///     {
///         directory: "themes",
///         path: "/data/themes",
///         rights: [ "r*" ],
///     },
///     {
///         storage: "persistent",
///         path: "/data",
///     },
///     {
///         event_stream: [
///             "started",
///             "stopped",
///         ],
///         from: "framework",
///     },
/// ],
/// ```
#[derive(Deserialize, Debug, Default, PartialEq, Clone, ReferenceDoc, Serialize)]
#[serde(deny_unknown_fields)]
#[reference_doc(fields_as = "list", top_level_doc_after_fields)]
pub struct Use {
    /// When using a service capability, the [name](#name) of a [service capability][doc-service].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<OneOrMany<Name>>,

    /// When using a protocol capability, the [name](#name) of a [protocol capability][doc-protocol].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<OneOrMany<Name>>,

    /// When using a directory capability, the [name](#name) of a [directory capability][doc-directory].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<Name>,

    /// When using a storage capability, the [name](#name) of a [storage capability][doc-storage].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<Name>,

    /// When using an event stream capability, the [name](#name) of an [event stream capability][doc-event].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_stream: Option<OneOrMany<Name>>,

    /// The source of the capability. Defaults to `parent`.  One of:
    /// - `parent`: The component's parent.
    /// - `debug`: One of [`debug_capabilities`][fidl-environment-decl] in the
    ///     environment assigned to this component.
    /// - `framework`: The Component Framework runtime.
    /// - `self`: This component.
    /// - `#<capability-name>`: The name of another capability from which the
    ///     requested capability is derived.
    /// - `#<child-name>`: A [reference](#references) to a child component
    ///     instance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<UseFromRef>,

    /// The path at which to install the capability in the component's namespace. For protocols,
    /// defaults to `/svc/${protocol}`.  Required for `directory` and `storage`. This property is
    /// disallowed for declarations with arrays of capability names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<Path>,

    /// (`directory` only) the maximum [directory rights][doc-directory-rights] to apply to
    /// the directory in the component's namespace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rights: Option<Rights>,

    /// (`directory` only) A subdirectory within the directory capability to provide in the
    /// component's namespace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdir: Option<RelativePath>,

    // TODO(fxbug.dev/81980): remove.
    /// Deprecated and unusable. In the process of being removed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#as: Option<Name>,

    /// (`event_stream` only) When defined the event stream will contain events about only the
    /// components defined in the scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<OneOrMany<EventScope>>,

    /// (`event_stream` only) Capability requested event streams require specifying a filter
    /// referring to the protocol to which the events in the event stream apply. The content of the
    /// filter will be an object mapping from "name" to the "protocol name".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<Map<String, Value>>,

    /// `dependency` _(optional)_: The type of dependency between the source and
    /// this component, one of:
    /// - `strong`: a strong dependency, which is used to determine shutdown
    ///     ordering. Component manager is guaranteed to stop the target before the
    ///     source. This is the default.
    /// - `weak`: a weak dependency, which is ignored during shutdown. When component manager
    ///     stops the parent realm, the source may stop before the clients. Clients of weak
    ///     dependencies must be able to handle these dependencies becoming unavailable.
    /// - `weak_for_migration`: this has the same runtime consequences as `weak`,
    ///     but also implies this capability will be made strong after completion of the v2
    ///     component migration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependency: Option<DependencyType>,

    /// `availability` _(optional)_: The expectations around this capability's availability. One
    /// of:
    /// - `required` (default): a required dependency, the component is unable to perform its
    ///     work without this capability.
    /// - `optional`: an optional dependency, the component will be able to function without this
    ///     capability (although if the capability is unavailable some functionality may be
    ///     disabled).
    /// - `transitional`: the source may omit the route completely without even having to route
    ///     from `void`. Used for soft transitions that introduce new capabilities.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub availability: Option<Availability>,
}

/// Example:
///
/// ```json5
/// expose: [
///     {
///         directory: "themes",
///         from: "self",
///     },
///     {
///         protocol: "pkg.Cache",
///         from: "#pkg_cache",
///         as: "fuchsia.pkg.PackageCache",
///     },
///     {
///         protocol: [
///             "fuchsia.ui.app.ViewProvider",
///             "fuchsia.fonts.Provider",
///         ],
///         from: "self",
///     },
///     {
///         runner: "web-chromium",
///         from: "#web_runner",
///         as: "web",
///     },
///     {
///         resolver: "full-resolver",
///         from: "#full-resolver",
///     },
/// ],
/// ```
#[derive(Deserialize, Debug, PartialEq, Clone, ReferenceDoc, Serialize)]
#[serde(deny_unknown_fields)]
#[reference_doc(fields_as = "list", top_level_doc_after_fields)]
pub struct Expose {
    /// When routing a service, the [name](#name) of a [service capability][doc-service].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<OneOrMany<Name>>,

    /// When routing a protocol, the [name](#name) of a [protocol capability][doc-protocol].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<OneOrMany<Name>>,

    /// When routing a directory, the [name](#name) of a [directory capability][doc-directory].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<OneOrMany<Name>>,

    /// When routing a runner, the [name](#name) of a [runner capability][doc-runners].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner: Option<OneOrMany<Name>>,

    /// When routing a resolver, the [name](#name) of a [resolver capability][doc-resolvers].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolver: Option<OneOrMany<Name>>,

    /// `from`: The source of the capability, one of:
    /// - `self`: This component. Requires a corresponding
    ///     [`capability`](#capabilities) declaration.
    /// - `framework`: The Component Framework runtime.
    /// - `#<child-name>`: A [reference](#references) to a child component
    ///     instance.
    pub from: OneOrMany<ExposeFromRef>,

    /// The [name](#name) for the capability as it will be known by the target. If omitted,
    /// defaults to the original name. `as` cannot be used when an array of multiple capability
    /// names is provided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#as: Option<Name>,

    /// The capability target. Either `parent` or `framework`. Defaults to `parent`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<ExposeToRef>,

    /// (`directory` only) the maximum [directory rights][doc-directory-rights] to apply to
    /// the exposed directory capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rights: Option<Rights>,

    /// (`directory` only) the relative path of a subdirectory within the source directory
    /// capability to route.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdir: Option<RelativePath>,

    /// (`event_stream` only) the name(s) of the event streams being exposed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_stream: Option<OneOrMany<Name>>,

    /// (`event_stream` only) the scope(s) of the event streams being exposed. This is used to
    /// downscope the range of components to which an event stream refers and make it refer only to
    /// the components defined in the scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<OneOrMany<EventScope>>,

    /// `availability` _(optional)_: The expectations around this capability's availability. Affects
    /// build-time and runtime route validation. One of:
    /// - `required` (default): a required dependency, the source must exist and provide it. Use
    ///     this when the target of this expose requires this capability to function properly.
    /// - `optional`: an optional dependency. Use this when the target of the expose can function
    ///     with or without this capability. The target must not have a `required` dependency on the
    ///     capability. The ultimate source of this expose must be `void` or an actual component.
    /// - `same_as_target`: the availability expectations of this capability will match the
    ///     target's. If the target requires the capability, then this field is set to `required`.
    ///     If the target has an optional dependency on the capability, then the field is set to
    ///     `optional`.
    /// - `transitional`: like `optional`, but will tolerate a missing source. Use this
    ///     only to avoid validation errors during transitional periods of multi-step code changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub availability: Option<Availability>,

    /// Whether or not the source of this offer must exist. One of:
    /// - `required` (default): the source (`from`) must be defined in this manifest.
    /// - `unknown`: the source of this offer will be rewritten to `void` if its source (`from`)
    ///     is not defined in this manifest after includes are processed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_availability: Option<SourceAvailability>,
}

impl Expose {
    pub fn new_from(from: OneOrMany<ExposeFromRef>) -> Self {
        Self {
            from,
            service: None,
            protocol: None,
            directory: None,
            runner: None,
            resolver: None,
            r#as: None,
            to: None,
            rights: None,
            subdir: None,
            event_stream: None,
            scope: None,
            availability: None,
            source_availability: None,
        }
    }
}

/// Example:
///
/// ```json5
/// offer: [
///     {
///         protocol: "fuchsia.logger.LogSink",
///         from: "#logger",
///         to: [ "#fshost", "#pkg_cache" ],
///         dependency: "weak_for_migration",
///     },
///     {
///         protocol: [
///             "fuchsia.ui.app.ViewProvider",
///             "fuchsia.fonts.Provider",
///         ],
///         from: "#session",
///         to: [ "#ui_shell" ],
///         dependency: "strong",
///     },
///     {
///         directory: "blobfs",
///         from: "self",
///         to: [ "#pkg_cache" ],
///     },
///     {
///         directory: "fshost-config",
///         from: "parent",
///         to: [ "#fshost" ],
///         as: "config",
///     },
///     {
///         storage: "cache",
///         from: "parent",
///         to: [ "#logger" ],
///     },
///     {
///         runner: "web",
///         from: "parent",
///         to: [ "#user-shell" ],
///     },
///     {
///         resolver: "full-resolver",
///         from: "parent",
///         to: [ "#user-shell" ],
///     },
///     {
///         event_stream: "stopped",
///         from: "framework",
///         to: [ "#logger" ],
///     },
/// ],
/// ```
#[derive(Deserialize, Debug, PartialEq, Clone, ReferenceDoc, Serialize)]
#[serde(deny_unknown_fields)]
#[reference_doc(fields_as = "list", top_level_doc_after_fields)]
pub struct Offer {
    /// When routing a service, the [name](#name) of a [service capability][doc-service].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service: Option<OneOrMany<Name>>,

    /// When routing a protocol, the [name](#name) of a [protocol capability][doc-protocol].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol: Option<OneOrMany<Name>>,

    /// When routing a directory, the [name](#name) of a [directory capability][doc-directory].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub directory: Option<OneOrMany<Name>>,

    /// When routing a runner, the [name](#name) of a [runner capability][doc-runners].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner: Option<OneOrMany<Name>>,

    /// When routing a resolver, the [name](#name) of a [resolver capability][doc-resolvers].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolver: Option<OneOrMany<Name>>,

    /// When routing a storage capability, the [name](#name) of a [storage capability][doc-storage].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<OneOrMany<Name>>,

    /// `from`: The source of the capability, one of:
    /// - `parent`: The component's parent. This source can be used for all
    ///     capability types.
    /// - `self`: This component. Requires a corresponding
    ///     [`capability`](#capabilities) declaration.
    /// - `framework`: The Component Framework runtime.
    /// - `#<child-name>`: A [reference](#references) to a child component
    ///     instance. This source can only be used when offering protocol,
    ///     directory, or runner capabilities.
    /// - `void`: The source is intentionally omitted. Only valid when `availability` is
    ///     `optional` or `transitional`.
    pub from: OneOrMany<OfferFromRef>,

    /// A capability target or array of targets, each of which is a [reference](#references) to the
    /// child or collection to which the capability is being offered, of the form `#<target-name>`.
    pub to: OneOrMany<OfferToRef>,

    /// An explicit [name](#name) for the capability as it will be known by the target. If omitted,
    /// defaults to the original name. `as` cannot be used when an array of multiple names is
    /// provided.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#as: Option<Name>,

    /// The type of dependency between the source and
    /// targets, one of:
    /// - `strong`: a strong dependency, which is used to determine shutdown
    ///     ordering. Component manager is guaranteed to stop the target before the
    ///     source. This is the default.
    /// - `weak_for_migration`: a weak dependency, which is ignored during
    ///     shutdown. When component manager stops the parent realm, the source may
    ///     stop before the clients. Clients of weak dependencies must be able to
    ///     handle these dependencies becoming unavailable. This type exists to keep
    ///     track of weak dependencies that resulted from migrations into v2
    ///     components.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependency: Option<DependencyType>,

    /// (`directory` only) the maximum [directory rights][doc-directory-rights] to apply to
    /// the offered directory capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rights: Option<Rights>,

    /// (`directory` only) the relative path of a subdirectory within the source directory
    /// capability to route.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdir: Option<RelativePath>,

    // TODO(fxbug.dev/81980): remove.
    /// Deprecated and unusable. In the process of being removed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<Map<String, Value>>,

    /// (`event_stream` only) the name(s) of the event streams being offered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_stream: Option<OneOrMany<Name>>,

    /// (`event_stream` only) When defined the event stream will contain events about only the
    /// components defined in the scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<OneOrMany<EventScope>>,

    /// `availability` _(optional)_: The expectations around this capability's availability. Affects
    /// build-time and runtime route validation. One of:
    /// - `required` (default): a required dependency, the source must exist and provide it. Use
    ///     this when the target of this offer requires this capability to function properly.
    /// - `optional`: an optional dependency. Use this when the target of the offer can function
    ///     with or without this capability. The target must not have a `required` dependency on the
    ///     capability. The ultimate source of this offer must be `void` or an actual component.
    /// - `same_as_target`: the availability expectations of this capability will match the
    ///     target's. If the target requires the capability, then this field is set to `required`.
    ///     If the target has an optional dependency on the capability, then the field is set to
    ///     `optional`.
    /// - `transitional`: like `optional`, but will tolerate a missing source. Use this
    ///     only to avoid validation errors during transitional periods of multi-step code changes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub availability: Option<Availability>,

    /// Whether or not the source of this offer must exist. One of:
    /// - `required` (default): the source (`from`) must be defined in this manifest.
    /// - `unknown`: the source of this offer will be rewritten to `void` if its source (`from`)
    ///     is not defined in this manifest after includes are processed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_availability: Option<SourceAvailability>,
}

/// Example:
///
/// ```json5
/// children: [
///     {
///         name: "logger",
///         url: "fuchsia-pkg://fuchsia.com/logger#logger.cm",
///     },
///     {
///         name: "pkg_cache",
///         url: "fuchsia-pkg://fuchsia.com/pkg_cache#meta/pkg_cache.cm",
///         startup: "eager",
///     },
///     {
///         name: "child",
///         url: "#meta/child.cm",
///     }
/// ],
/// ```
///
/// [component-url]: /docs/reference/components/url.md
/// [doc-eager]: /docs/development/components/connect.md#eager
/// [doc-reboot-on-terminate]: /docs/development/components/connect.md#reboot-on-terminate
#[derive(ReferenceDoc, Deserialize, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[reference_doc(fields_as = "list", top_level_doc_after_fields)]
pub struct Child {
    /// The name of the child component instance, which is a string of one
    /// or more of the following characters: `a-z`, `0-9`, `_`, `.`, `-`. The name
    /// identifies this component when used in a [reference](#references).
    pub name: Name,

    /// The [component URL][component-url] for the child component instance.
    pub url: Url,

    /// The component instance's startup mode. One of:
    /// - `lazy` _(default)_: Start the component instance only if another
    ///     component instance binds to it.
    /// - [`eager`][doc-eager]: Start the component instance as soon as its parent
    ///     starts.
    #[serde(default)]
    #[serde(skip_serializing_if = "StartupMode::is_lazy")]
    pub startup: StartupMode,

    /// Determines the fault recovery policy to apply if this component terminates.
    /// - `none` _(default)_: Do nothing.
    /// - `reboot`: Gracefully reboot the system if the component terminates for
    ///     any reason. This is a special feature for use only by a narrow set of
    ///     components; see [Termination policies][doc-reboot-on-terminate] for more
    ///     information.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_terminate: Option<OnTerminate>,

    /// If present, the name of the environment to be assigned to the child component instance, one
    /// of [`environments`](#environments). If omitted, the child will inherit the same environment
    /// assigned to this component.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<EnvironmentRef>,
}

/// Example:
///
/// ```json5
/// disable: {
///     must_offer_protocol: [ "fuchsia.logger.LogSink", "fuchsia.component.Binder" ],
///     must_use_protocol: [ "fuchsia.logger.LogSink" ],
/// }
/// ```
#[derive(Default, ReferenceDoc, Deserialize, Debug, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
#[reference_doc(fields_as = "list", top_level_doc_after_fields)]
pub struct Disable {
    /// Lists protocols for which the option "--must-offer-protocol" is disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub must_offer_protocol: Option<Vec<String>>,

    /// Lists protocols for which the option "--must-use-protocol" is disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub must_use_protocol: Option<Vec<String>>,
}

#[derive(Deserialize, Debug, PartialEq, ReferenceDoc, Serialize)]
#[serde(deny_unknown_fields)]
#[reference_doc(fields_as = "list", top_level_doc_after_fields)]
/// Example:
///
/// ```json5
/// collections: [
///     {
///         name: "tests",
///         durability: "transient",
///     },
/// ],
/// ```
pub struct Collection {
    /// The name of the component collection, which is a string of one or
    /// more of the following characters: `a-z`, `0-9`, `_`, `.`, `-`. The name
    /// identifies this collection when used in a [reference](#references).
    pub name: Name,

    /// The duration of child component instances in the collection.
    /// - `transient`: The instance exists until its parent is stopped or it is
    ///     explicitly destroyed.
    /// - `single_run`: The instance is started when it is created, and destroyed
    ///     when it is stopped.
    pub durability: Durability,

    /// If present, the environment that will be
    /// assigned to instances in this collection, one of
    /// [`environments`](#environments). If omitted, instances in this collection
    /// will inherit the same environment assigned to this component.
    pub environment: Option<EnvironmentRef>,

    /// Constraints on the dynamic offers that target the components in this collection.
    /// Dynamic offers are specified when calling `fuchsia.component.Realm/CreateChild`.
    /// - `static_only`: Only those specified in this `.cml` file. No dynamic offers.
    ///     This is the default.
    /// - `static_and_dynamic`: Both static offers and those specified at runtime
    ///     with `CreateChild` are allowed.
    pub allowed_offers: Option<AllowedOffers>,

    /// Allow child names up to 1024 characters long instead of the usual 100 character limit.
    /// Default is false.
    pub allow_long_names: Option<bool>,

    /// If set to `true`, the data in isolated storage used by dynamic child instances and
    /// their descendants will persist after the instances are destroyed. A new child instance
    /// created with the same name will share the same storage path as the previous instance.
    pub persistent_storage: Option<bool>,
}

pub trait FromClause {
    fn from_(&self) -> OneOrMany<AnyRef<'_>>;
}

pub trait CapabilityClause: Clone + PartialEq {
    fn service(&self) -> Option<OneOrMany<Name>>;
    fn protocol(&self) -> Option<OneOrMany<Name>>;
    fn directory(&self) -> Option<OneOrMany<Name>>;
    fn storage(&self) -> Option<OneOrMany<Name>>;
    fn runner(&self) -> Option<OneOrMany<Name>>;
    fn resolver(&self) -> Option<OneOrMany<Name>>;
    fn event_stream(&self) -> Option<OneOrMany<Name>>;
    fn set_service(&mut self, o: Option<OneOrMany<Name>>);
    fn set_protocol(&mut self, o: Option<OneOrMany<Name>>);
    fn set_directory(&mut self, o: Option<OneOrMany<Name>>);
    fn set_storage(&mut self, o: Option<OneOrMany<Name>>);
    fn set_runner(&mut self, o: Option<OneOrMany<Name>>);
    fn set_resolver(&mut self, o: Option<OneOrMany<Name>>);
    fn set_event_stream(&mut self, o: Option<OneOrMany<Name>>);

    fn availability(&self) -> Option<Availability>;
    fn set_availability(&mut self, a: Option<Availability>);

    /// Returns the name of the capability for display purposes.
    /// If `service()` returns `Some`, the capability name must be "service", etc.
    ///
    /// Panics if a capability keyword is not set.
    fn capability_type(&self) -> &'static str;

    fn decl_type(&self) -> &'static str;
    fn supported(&self) -> &[&'static str];

    /// Returns the names of the capabilities in this clause.
    /// If `protocol()` returns `Some(OneOrMany::Many(vec!["a", "b"]))`, this returns!["a", "b"].
    fn names(&self) -> Vec<Name> {
        let res = vec![
            self.service(),
            self.protocol(),
            self.directory(),
            self.storage(),
            self.runner(),
            self.resolver(),
            self.event_stream(),
        ];
        res.into_iter()
            .map(|o| o.map(|o| o.into_iter().collect::<Vec<Name>>()).unwrap_or(vec![]))
            .flatten()
            .collect()
    }
}

pub trait AsClause {
    fn r#as(&self) -> Option<&Name>;
}

pub trait PathClause {
    fn path(&self) -> Option<&Path>;
}

pub trait FilterClause {
    fn filter(&self) -> Option<&Map<String, Value>>;
}

pub trait RightsClause {
    fn rights(&self) -> Option<&Rights>;
}

fn always_one<T>(o: Option<OneOrMany<T>>) -> Option<T> {
    o.map(|o| match o {
        OneOrMany::One(o) => o,
        OneOrMany::Many(_) => panic!("many is impossible"),
    })
}

impl CapabilityClause for Capability {
    fn service(&self) -> Option<OneOrMany<Name>> {
        self.service.clone()
    }
    fn protocol(&self) -> Option<OneOrMany<Name>> {
        self.protocol.clone()
    }
    fn directory(&self) -> Option<OneOrMany<Name>> {
        self.directory.as_ref().map(|n| OneOrMany::One(n.clone()))
    }
    fn storage(&self) -> Option<OneOrMany<Name>> {
        self.storage.as_ref().map(|n| OneOrMany::One(n.clone()))
    }
    fn runner(&self) -> Option<OneOrMany<Name>> {
        self.runner.as_ref().map(|n| OneOrMany::One(n.clone()))
    }
    fn resolver(&self) -> Option<OneOrMany<Name>> {
        self.resolver.as_ref().map(|n| OneOrMany::One(n.clone()))
    }
    fn event_stream(&self) -> Option<OneOrMany<Name>> {
        self.event_stream.clone()
    }

    fn set_service(&mut self, o: Option<OneOrMany<Name>>) {
        self.service = o;
    }
    fn set_protocol(&mut self, o: Option<OneOrMany<Name>>) {
        self.protocol = o;
    }
    fn set_directory(&mut self, o: Option<OneOrMany<Name>>) {
        self.directory = always_one(o);
    }
    fn set_storage(&mut self, o: Option<OneOrMany<Name>>) {
        self.storage = always_one(o);
    }
    fn set_runner(&mut self, o: Option<OneOrMany<Name>>) {
        self.runner = always_one(o);
    }
    fn set_resolver(&mut self, o: Option<OneOrMany<Name>>) {
        self.resolver = always_one(o);
    }
    fn set_event_stream(&mut self, o: Option<OneOrMany<Name>>) {
        self.event_stream = o;
    }

    fn availability(&self) -> Option<Availability> {
        None
    }
    fn set_availability(&mut self, _a: Option<Availability>) {}

    fn capability_type(&self) -> &'static str {
        if self.service.is_some() {
            "service"
        } else if self.protocol.is_some() {
            "protocol"
        } else if self.directory.is_some() {
            "directory"
        } else if self.storage.is_some() {
            "storage"
        } else if self.runner.is_some() {
            "runner"
        } else if self.resolver.is_some() {
            "resolver"
        } else if self.event_stream.is_some() {
            "event_stream"
        } else {
            panic!("Missing capability name")
        }
    }
    fn decl_type(&self) -> &'static str {
        "capability"
    }
    fn supported(&self) -> &[&'static str] {
        &["service", "protocol", "directory", "storage", "runner", "resolver", "event_stream"]
    }
}

impl AsClause for Capability {
    fn r#as(&self) -> Option<&Name> {
        None
    }
}

impl PathClause for Capability {
    fn path(&self) -> Option<&Path> {
        self.path.as_ref()
    }
}

impl FilterClause for Capability {
    fn filter(&self) -> Option<&Map<String, Value>> {
        None
    }
}

impl RightsClause for Capability {
    fn rights(&self) -> Option<&Rights> {
        self.rights.as_ref()
    }
}

impl CapabilityClause for DebugRegistration {
    fn service(&self) -> Option<OneOrMany<Name>> {
        None
    }
    fn protocol(&self) -> Option<OneOrMany<Name>> {
        self.protocol.clone()
    }
    fn directory(&self) -> Option<OneOrMany<Name>> {
        None
    }
    fn storage(&self) -> Option<OneOrMany<Name>> {
        None
    }
    fn runner(&self) -> Option<OneOrMany<Name>> {
        None
    }
    fn resolver(&self) -> Option<OneOrMany<Name>> {
        None
    }
    fn event_stream(&self) -> Option<OneOrMany<Name>> {
        None
    }

    fn set_service(&mut self, _o: Option<OneOrMany<Name>>) {}
    fn set_protocol(&mut self, o: Option<OneOrMany<Name>>) {
        self.protocol = o;
    }
    fn set_directory(&mut self, _o: Option<OneOrMany<Name>>) {}
    fn set_storage(&mut self, _o: Option<OneOrMany<Name>>) {}
    fn set_runner(&mut self, _o: Option<OneOrMany<Name>>) {}
    fn set_resolver(&mut self, _o: Option<OneOrMany<Name>>) {}
    fn set_event_stream(&mut self, _o: Option<OneOrMany<Name>>) {}

    fn availability(&self) -> Option<Availability> {
        None
    }
    fn set_availability(&mut self, _a: Option<Availability>) {}

    fn capability_type(&self) -> &'static str {
        if self.protocol.is_some() {
            "protocol"
        } else {
            panic!("Missing capability name")
        }
    }
    fn decl_type(&self) -> &'static str {
        "debug"
    }
    fn supported(&self) -> &[&'static str] {
        &["service", "protocol"]
    }
}

impl AsClause for DebugRegistration {
    fn r#as(&self) -> Option<&Name> {
        self.r#as.as_ref()
    }
}

impl PathClause for DebugRegistration {
    fn path(&self) -> Option<&Path> {
        None
    }
}

impl FromClause for DebugRegistration {
    fn from_(&self) -> OneOrMany<AnyRef<'_>> {
        OneOrMany::One(AnyRef::from(&self.from))
    }
}

impl CapabilityClause for Use {
    fn service(&self) -> Option<OneOrMany<Name>> {
        self.service.clone()
    }
    fn protocol(&self) -> Option<OneOrMany<Name>> {
        self.protocol.clone()
    }
    fn directory(&self) -> Option<OneOrMany<Name>> {
        self.directory.as_ref().map(|n| OneOrMany::One(n.clone()))
    }
    fn storage(&self) -> Option<OneOrMany<Name>> {
        self.storage.as_ref().map(|n| OneOrMany::One(n.clone()))
    }
    fn runner(&self) -> Option<OneOrMany<Name>> {
        None
    }
    fn resolver(&self) -> Option<OneOrMany<Name>> {
        None
    }
    fn event_stream(&self) -> Option<OneOrMany<Name>> {
        self.event_stream.clone()
    }

    fn set_service(&mut self, o: Option<OneOrMany<Name>>) {
        self.service = o;
    }
    fn set_protocol(&mut self, o: Option<OneOrMany<Name>>) {
        self.protocol = o;
    }
    fn set_directory(&mut self, o: Option<OneOrMany<Name>>) {
        self.directory = always_one(o);
    }
    fn set_storage(&mut self, o: Option<OneOrMany<Name>>) {
        self.storage = always_one(o);
    }
    fn set_runner(&mut self, _o: Option<OneOrMany<Name>>) {}
    fn set_resolver(&mut self, _o: Option<OneOrMany<Name>>) {}
    fn set_event_stream(&mut self, o: Option<OneOrMany<Name>>) {
        self.event_stream = o;
    }

    fn availability(&self) -> Option<Availability> {
        self.availability
    }
    fn set_availability(&mut self, a: Option<Availability>) {
        self.availability = a;
    }

    fn capability_type(&self) -> &'static str {
        if self.service.is_some() {
            "service"
        } else if self.protocol.is_some() {
            "protocol"
        } else if self.directory.is_some() {
            "directory"
        } else if self.storage.is_some() {
            "storage"
        } else if self.event_stream.is_some() {
            "event_stream"
        } else {
            panic!("Missing capability name")
        }
    }
    fn decl_type(&self) -> &'static str {
        "use"
    }
    fn supported(&self) -> &[&'static str] {
        &["service", "protocol", "directory", "storage", "event_stream"]
    }
}

impl FilterClause for Use {
    fn filter(&self) -> Option<&Map<String, Value>> {
        self.filter.as_ref()
    }
}

impl AsClause for Use {
    fn r#as(&self) -> Option<&Name> {
        self.r#as.as_ref()
    }
}

impl PathClause for Use {
    fn path(&self) -> Option<&Path> {
        self.path.as_ref()
    }
}

impl FromClause for Expose {
    fn from_(&self) -> OneOrMany<AnyRef<'_>> {
        one_or_many_from_impl(&self.from)
    }
}

impl RightsClause for Use {
    fn rights(&self) -> Option<&Rights> {
        self.rights.as_ref()
    }
}

impl CapabilityClause for Expose {
    fn service(&self) -> Option<OneOrMany<Name>> {
        self.service.as_ref().map(|n| n.clone())
    }
    fn protocol(&self) -> Option<OneOrMany<Name>> {
        self.protocol.clone()
    }
    fn directory(&self) -> Option<OneOrMany<Name>> {
        self.directory.clone()
    }
    fn storage(&self) -> Option<OneOrMany<Name>> {
        None
    }
    fn runner(&self) -> Option<OneOrMany<Name>> {
        self.runner.clone()
    }
    fn resolver(&self) -> Option<OneOrMany<Name>> {
        self.resolver.clone()
    }
    fn event_stream(&self) -> Option<OneOrMany<Name>> {
        self.event_stream.clone()
    }

    fn set_service(&mut self, o: Option<OneOrMany<Name>>) {
        self.service = o;
    }
    fn set_protocol(&mut self, o: Option<OneOrMany<Name>>) {
        self.protocol = o;
    }
    fn set_directory(&mut self, o: Option<OneOrMany<Name>>) {
        self.directory = o;
    }
    fn set_storage(&mut self, _o: Option<OneOrMany<Name>>) {}
    fn set_runner(&mut self, o: Option<OneOrMany<Name>>) {
        self.runner = o;
    }
    fn set_resolver(&mut self, o: Option<OneOrMany<Name>>) {
        self.resolver = o;
    }
    fn set_event_stream(&mut self, o: Option<OneOrMany<Name>>) {
        self.event_stream = o;
    }

    fn availability(&self) -> Option<Availability> {
        None
    }
    fn set_availability(&mut self, _a: Option<Availability>) {}

    fn capability_type(&self) -> &'static str {
        if self.service.is_some() {
            "service"
        } else if self.protocol.is_some() {
            "protocol"
        } else if self.directory.is_some() {
            "directory"
        } else if self.runner.is_some() {
            "runner"
        } else if self.resolver.is_some() {
            "resolver"
        } else if self.event_stream.is_some() {
            "event_stream"
        } else {
            panic!("Missing capability name")
        }
    }
    fn decl_type(&self) -> &'static str {
        "expose"
    }
    fn supported(&self) -> &[&'static str] {
        &["service", "protocol", "directory", "runner", "resolver", "event_stream"]
    }
}

impl AsClause for Expose {
    fn r#as(&self) -> Option<&Name> {
        self.r#as.as_ref()
    }
}

impl PathClause for Expose {
    fn path(&self) -> Option<&Path> {
        None
    }
}

impl FilterClause for Expose {
    fn filter(&self) -> Option<&Map<String, Value>> {
        None
    }
}

impl RightsClause for Expose {
    fn rights(&self) -> Option<&Rights> {
        self.rights.as_ref()
    }
}

impl FromClause for Offer {
    fn from_(&self) -> OneOrMany<AnyRef<'_>> {
        one_or_many_from_impl(&self.from)
    }
}

impl CapabilityClause for Offer {
    fn service(&self) -> Option<OneOrMany<Name>> {
        self.service.as_ref().map(|n| n.clone())
    }
    fn protocol(&self) -> Option<OneOrMany<Name>> {
        self.protocol.clone()
    }
    fn directory(&self) -> Option<OneOrMany<Name>> {
        self.directory.clone()
    }
    fn storage(&self) -> Option<OneOrMany<Name>> {
        self.storage.clone()
    }
    fn runner(&self) -> Option<OneOrMany<Name>> {
        self.runner.clone()
    }
    fn resolver(&self) -> Option<OneOrMany<Name>> {
        self.resolver.clone()
    }
    fn event_stream(&self) -> Option<OneOrMany<Name>> {
        self.event_stream.clone()
    }

    fn set_service(&mut self, o: Option<OneOrMany<Name>>) {
        self.service = o;
    }
    fn set_protocol(&mut self, o: Option<OneOrMany<Name>>) {
        self.protocol = o;
    }
    fn set_directory(&mut self, o: Option<OneOrMany<Name>>) {
        self.directory = o;
    }
    fn set_storage(&mut self, o: Option<OneOrMany<Name>>) {
        self.storage = o;
    }
    fn set_runner(&mut self, o: Option<OneOrMany<Name>>) {
        self.runner = o;
    }
    fn set_resolver(&mut self, o: Option<OneOrMany<Name>>) {
        self.resolver = o;
    }
    fn set_event_stream(&mut self, o: Option<OneOrMany<Name>>) {
        self.event_stream = o;
    }

    fn availability(&self) -> Option<Availability> {
        self.availability
    }
    fn set_availability(&mut self, a: Option<Availability>) {
        self.availability = a;
    }

    fn capability_type(&self) -> &'static str {
        if self.service.is_some() {
            "service"
        } else if self.protocol.is_some() {
            "protocol"
        } else if self.directory.is_some() {
            "directory"
        } else if self.storage.is_some() {
            "storage"
        } else if self.runner.is_some() {
            "runner"
        } else if self.resolver.is_some() {
            "resolver"
        } else if self.event_stream.is_some() {
            "event_stream"
        } else {
            panic!("Missing capability name")
        }
    }
    fn decl_type(&self) -> &'static str {
        "offer"
    }
    fn supported(&self) -> &[&'static str] {
        &["service", "protocol", "directory", "storage", "runner", "resolver", "event_stream"]
    }
}

impl AsClause for Offer {
    fn r#as(&self) -> Option<&Name> {
        self.r#as.as_ref()
    }
}

impl PathClause for Offer {
    fn path(&self) -> Option<&Path> {
        None
    }
}

impl FilterClause for Offer {
    fn filter(&self) -> Option<&Map<String, Value>> {
        self.filter.as_ref()
    }
}

impl RightsClause for Offer {
    fn rights(&self) -> Option<&Rights> {
        self.rights.as_ref()
    }
}

impl FromClause for RunnerRegistration {
    fn from_(&self) -> OneOrMany<AnyRef<'_>> {
        OneOrMany::One(AnyRef::from(&self.from))
    }
}

impl FromClause for ResolverRegistration {
    fn from_(&self) -> OneOrMany<AnyRef<'_>> {
        OneOrMany::One(AnyRef::from(&self.from))
    }
}

fn one_or_many_from_impl<'a, T>(from: &'a OneOrMany<T>) -> OneOrMany<AnyRef<'a>>
where
    AnyRef<'a>: From<&'a T>,
    T: 'a,
{
    let r = match from {
        OneOrMany::One(r) => OneOrMany::One(r.into()),
        OneOrMany::Many(v) => OneOrMany::Many(v.into_iter().map(|r| r.into()).collect()),
    };
    r.into()
}

pub fn alias_or_name(alias: Option<&Name>, name: &Name) -> Name {
    alias.unwrap_or(name).clone()
}

pub fn alias_or_path(alias: Option<&Path>, path: &Path) -> Path {
    alias.unwrap_or(path).clone()
}

pub fn format_cml(buffer: &str, file: &std::path::Path) -> Result<Vec<u8>, Error> {
    let general_order = PathOption::PropertyNameOrder(vec![
        "name",
        "url",
        "startup",
        "environment",
        "durability",
        "service",
        "protocol",
        "directory",
        "storage",
        "runner",
        "resolver",
        "event",
        "event_stream",
        "from",
        "as",
        "to",
        "rights",
        "path",
        "subdir",
        "filter",
        "dependency",
        "extends",
        "runners",
        "resolvers",
        "debug",
    ]);
    let options = FormatOptions {
        collapse_containers_of_one: true,
        sort_array_items: true, // but use options_by_path to turn this off for program args
        options_by_path: hashmap! {
            "/*" => hashset! {
                PathOption::PropertyNameOrder(vec![
                    "include",
                    "disable",
                    "program",
                    "children",
                    "collections",
                    "capabilities",
                    "use",
                    "offer",
                    "expose",
                    "environments",
                    "facets",
                ])
            },
            "/*/program" => hashset! {
                PathOption::CollapseContainersOfOne(false),
                PathOption::PropertyNameOrder(vec![
                    "runner",
                    "binary",
                    "args",
                ]),
            },
            "/*/program/*" => hashset! {
                PathOption::SortArrayItems(false),
            },
            "/*/*/*" => hashset! {
                general_order.clone()
            },
            "/*/*/*/*/*" => hashset! {
                general_order
            },
        },
        ..Default::default()
    };

    json5format::format(buffer, Some(file.to_string_lossy().to_string()), Some(options))
        .map_err(|e| Error::json5(e, file))
}

pub fn offer_to_all_and_component_diff_sources_message(
    protocol: &[&str],
    component: &str,
) -> String {
    format!(
        r#"Protocol {:?} is offered to both "all" and "{}" with different sources"#,
        protocol, component
    )
}

pub fn offer_to_all_and_component_diff_protocols_message(
    protocol: &[&str],
    component: &str,
) -> String {
    format!(
        r#"Protocol {:?} is aliased to "{}" with the same name as an offer to "all", but from different source protocols"#,
        protocol, component
    )
}

/// Returns `Ok(true)` if desugaring the `offer_to_all` using `name` duplicates
/// `specific_offer`. Returns `Ok(false)` if not a duplicate.
///
/// Returns Err if there is a validation error.
pub fn offer_to_all_would_duplicate(
    offer_to_all: &Offer,
    specific_offer: &Offer,
    target: &cm_types::Name,
) -> Result<bool, Error> {
    // Only protocols may be offered to all
    assert!(offer_to_all.protocol.is_some());

    // If none of the pairs of the cross products of the two offer's protocols
    // match, then the offer is certainly not a duplicate
    if CapabilityId::from_offer_expose(specific_offer).iter().flatten().all(
        |specific_offer_cap_id| {
            CapabilityId::from_offer_expose(offer_to_all)
                .iter()
                .flatten()
                .all(|offer_to_all_cap_id| offer_to_all_cap_id != specific_offer_cap_id)
        },
    ) {
        return Ok(false);
    }

    let to_field_matches = specific_offer
        .to
        .iter()
        .any(|specific_offer_to| matches!(specific_offer_to, OfferToRef::Named(c) if c == target));

    if !to_field_matches {
        return Ok(false);
    }

    if offer_to_all.from != specific_offer.from {
        return Err(Error::validate(offer_to_all_and_component_diff_sources_message(
            offer_to_all
                .protocol
                .as_ref()
                .unwrap()
                .iter()
                .map(Name::as_str)
                .collect::<Vec<_>>()
                .as_ref(),
            target.as_str(),
        )));
    }

    // Since the capability ID's match, the underlying protocol must also match
    if offer_to_all.protocol.as_ref().unwrap().iter().all(|to_all_protocol| {
        specific_offer
            .protocol
            .as_ref()
            .unwrap()
            .iter()
            .all(|to_specific_protocol| to_all_protocol != to_specific_protocol)
    }) {
        return Err(Error::validate(offer_to_all_and_component_diff_protocols_message(
            offer_to_all
                .protocol
                .as_ref()
                .unwrap()
                .iter()
                .map(Name::as_str)
                .collect::<Vec<_>>()
                .as_ref(),
            target.as_str(),
        )));
    }

    Ok(true)
}

#[cfg(test)]
pub fn create_offer(
    protocol_name: &str,
    from: OneOrMany<OfferFromRef>,
    to: OneOrMany<OfferToRef>,
) -> Offer {
    Offer {
        protocol: Some(OneOrMany::One(Name::from_str(protocol_name).unwrap())),
        from,
        to,
        r#as: None,
        service: None,
        directory: None,
        runner: None,
        resolver: None,
        storage: None,
        dependency: None,
        rights: None,
        subdir: None,
        filter: None,
        event_stream: None,
        scope: None,
        availability: None,
        source_availability: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use {
        assert_matches::assert_matches,
        cm_json::{self, Error as JsonError},
        error::Error,
        serde_json::{self, json},
        serde_json5,
        std::path::Path,
        test_case::test_case,
    };

    // Exercise reference parsing tests on `OfferFromRef` because it contains every reference
    // subtype.

    #[test]
    fn test_parse_named_reference() {
        assert_matches!("#some-child".parse::<OfferFromRef>(), Ok(OfferFromRef::Named(name)) if name == "some-child");
        assert_matches!("#A".parse::<OfferFromRef>(), Ok(OfferFromRef::Named(name)) if name == "A");
        assert_matches!("#7".parse::<OfferFromRef>(), Ok(OfferFromRef::Named(name)) if name == "7");
        assert_matches!("#_".parse::<OfferFromRef>(), Ok(OfferFromRef::Named(name)) if name == "_");

        assert_matches!("#-".parse::<OfferFromRef>(), Err(_));
        assert_matches!("#.".parse::<OfferFromRef>(), Err(_));
        assert_matches!("#".parse::<OfferFromRef>(), Err(_));
        assert_matches!("some-child".parse::<OfferFromRef>(), Err(_));
    }

    #[test]
    fn test_parse_reference_test() {
        assert_matches!("parent".parse::<OfferFromRef>(), Ok(OfferFromRef::Parent));
        assert_matches!("framework".parse::<OfferFromRef>(), Ok(OfferFromRef::Framework));
        assert_matches!("self".parse::<OfferFromRef>(), Ok(OfferFromRef::Self_));
        assert_matches!("#child".parse::<OfferFromRef>(), Ok(OfferFromRef::Named(name)) if name == "child");

        assert_matches!("invalid".parse::<OfferFromRef>(), Err(_));
        assert_matches!("#invalid-child^".parse::<OfferFromRef>(), Err(_));
    }

    fn parse_as_ref(input: &str) -> Result<OfferFromRef, JsonError> {
        serde_json::from_value::<OfferFromRef>(cm_json::from_json_str(
            input,
            &Path::new("test.cml"),
        )?)
        .map_err(|e| JsonError::parse(format!("{}", e), None, None))
    }

    #[test]
    fn test_deserialize_ref() -> Result<(), JsonError> {
        assert_matches!(parse_as_ref("\"self\""), Ok(OfferFromRef::Self_));
        assert_matches!(parse_as_ref("\"parent\""), Ok(OfferFromRef::Parent));
        assert_matches!(parse_as_ref("\"#child\""), Ok(OfferFromRef::Named(name)) if name == "child");

        assert_matches!(parse_as_ref(r#""invalid""#), Err(_));

        Ok(())
    }

    macro_rules! test_parse_rights {
        (
            $(
                ($input:expr, $expected:expr),
            )+
        ) => {
            #[test]
            fn parse_rights() {
                $(
                    parse_rights_test($input, $expected);
                )+
            }
        }
    }

    fn parse_rights_test(input: &str, expected: Right) {
        let r: Right = serde_json5::from_str(&format!("\"{}\"", input)).expect("invalid json");
        assert_eq!(r, expected);
    }

    test_parse_rights! {
        ("connect", Right::Connect),
        ("enumerate", Right::Enumerate),
        ("execute", Right::Execute),
        ("get_attributes", Right::GetAttributes),
        ("modify_directory", Right::ModifyDirectory),
        ("read_bytes", Right::ReadBytes),
        ("traverse", Right::Traverse),
        ("update_attributes", Right::UpdateAttributes),
        ("write_bytes", Right::WriteBytes),
        ("r*", Right::ReadAlias),
        ("w*", Right::WriteAlias),
        ("x*", Right::ExecuteAlias),
        ("rw*", Right::ReadWriteAlias),
        ("rx*", Right::ReadExecuteAlias),
    }

    macro_rules! test_expand_rights {
        (
            $(
                ($input:expr, $expected:expr),
            )+
        ) => {
            #[test]
            fn expand_rights() {
                $(
                    expand_rights_test($input, $expected);
                )+
            }
        }
    }

    fn expand_rights_test(input: Right, expected: Vec<fio::Operations>) {
        assert_eq!(input.expand(), expected);
    }

    test_expand_rights! {
        (Right::Connect, vec![fio::Operations::CONNECT]),
        (Right::Enumerate, vec![fio::Operations::ENUMERATE]),
        (Right::Execute, vec![fio::Operations::EXECUTE]),
        (Right::GetAttributes, vec![fio::Operations::GET_ATTRIBUTES]),
        (Right::ModifyDirectory, vec![fio::Operations::MODIFY_DIRECTORY]),
        (Right::ReadBytes, vec![fio::Operations::READ_BYTES]),
        (Right::Traverse, vec![fio::Operations::TRAVERSE]),
        (Right::UpdateAttributes, vec![fio::Operations::UPDATE_ATTRIBUTES]),
        (Right::WriteBytes, vec![fio::Operations::WRITE_BYTES]),
        (Right::ReadAlias, vec![
            fio::Operations::CONNECT,
            fio::Operations::ENUMERATE,
            fio::Operations::TRAVERSE,
            fio::Operations::READ_BYTES,
            fio::Operations::GET_ATTRIBUTES,
        ]),
        (Right::WriteAlias, vec![
            fio::Operations::CONNECT,
            fio::Operations::ENUMERATE,
            fio::Operations::TRAVERSE,
            fio::Operations::WRITE_BYTES,
            fio::Operations::MODIFY_DIRECTORY,
            fio::Operations::UPDATE_ATTRIBUTES,
        ]),
        (Right::ExecuteAlias, vec![
            fio::Operations::CONNECT,
            fio::Operations::ENUMERATE,
            fio::Operations::TRAVERSE,
            fio::Operations::EXECUTE,
        ]),
        (Right::ReadWriteAlias, vec![
            fio::Operations::CONNECT,
            fio::Operations::ENUMERATE,
            fio::Operations::TRAVERSE,
            fio::Operations::READ_BYTES,
            fio::Operations::WRITE_BYTES,
            fio::Operations::MODIFY_DIRECTORY,
            fio::Operations::GET_ATTRIBUTES,
            fio::Operations::UPDATE_ATTRIBUTES,
        ]),
        (Right::ReadExecuteAlias, vec![
            fio::Operations::CONNECT,
            fio::Operations::ENUMERATE,
            fio::Operations::TRAVERSE,
            fio::Operations::READ_BYTES,
            fio::Operations::GET_ATTRIBUTES,
            fio::Operations::EXECUTE,
        ]),
    }

    #[test]
    fn test_deny_unknown_fields() {
        assert_matches!(serde_json5::from_str::<Document>("{ unknown: \"\" }"), Err(_));
        assert_matches!(serde_json5::from_str::<Environment>("{ unknown: \"\" }"), Err(_));
        assert_matches!(serde_json5::from_str::<RunnerRegistration>("{ unknown: \"\" }"), Err(_));
        assert_matches!(serde_json5::from_str::<ResolverRegistration>("{ unknown: \"\" }"), Err(_));
        assert_matches!(serde_json5::from_str::<Use>("{ unknown: \"\" }"), Err(_));
        assert_matches!(serde_json5::from_str::<Expose>("{ unknown: \"\" }"), Err(_));
        assert_matches!(serde_json5::from_str::<Offer>("{ unknown: \"\" }"), Err(_));
        assert_matches!(serde_json5::from_str::<Capability>("{ unknown: \"\" }"), Err(_));
        assert_matches!(serde_json5::from_str::<Child>("{ unknown: \"\" }"), Err(_));
        assert_matches!(serde_json5::from_str::<Collection>("{ unknown: \"\" }"), Err(_));
    }

    // TODO: Use Default::default() instead

    fn empty_offer() -> Offer {
        Offer {
            service: None,
            protocol: None,
            directory: None,
            storage: None,
            runner: None,
            resolver: None,
            from: OneOrMany::One(OfferFromRef::Self_),
            to: OneOrMany::Many(vec![]),
            r#as: None,
            rights: None,
            subdir: None,
            dependency: None,
            filter: None,
            event_stream: None,
            scope: None,
            availability: None,
            source_availability: None,
        }
    }

    fn empty_use() -> Use {
        Use {
            service: None,
            protocol: None,
            scope: None,
            directory: None,
            storage: None,
            from: None,
            path: None,
            r#as: None,
            rights: None,
            subdir: None,
            event_stream: None,
            filter: None,
            dependency: None,
            availability: None,
        }
    }

    #[test]
    fn test_capability_id() -> Result<(), Error> {
        // service
        assert_eq!(
            CapabilityId::from_offer_expose(&Offer {
                service: Some(OneOrMany::One("a".parse().unwrap())),
                ..empty_offer()
            },)?,
            vec![CapabilityId::Service("a".parse().unwrap())]
        );
        assert_eq!(
            CapabilityId::from_offer_expose(&Offer {
                service: Some(OneOrMany::Many(vec!["a".parse().unwrap(), "b".parse().unwrap()],)),
                ..empty_offer()
            },)?,
            vec![
                CapabilityId::Service("a".parse().unwrap()),
                CapabilityId::Service("b".parse().unwrap())
            ]
        );
        assert_eq!(
            CapabilityId::from_use(&Use {
                service: Some(OneOrMany::One("a".parse().unwrap())),
                ..empty_use()
            },)?,
            vec![CapabilityId::UsedService("/svc/a".parse().unwrap())]
        );
        assert_eq!(
            CapabilityId::from_use(&Use {
                service: Some(OneOrMany::Many(vec!["a".parse().unwrap(), "b".parse().unwrap(),],)),
                ..empty_use()
            },)?,
            vec![
                CapabilityId::UsedService("/svc/a".parse().unwrap()),
                CapabilityId::UsedService("/svc/b".parse().unwrap())
            ]
        );
        assert_eq!(
            CapabilityId::from_use(&Use {
                event_stream: Some(OneOrMany::One(Name::try_new("test".to_string()).unwrap())),
                path: Some(cm_types::Path::new("/svc/myevent".to_string()).unwrap()),
                ..empty_use()
            },)?,
            vec![CapabilityId::UsedEventStream("/svc/myevent".parse().unwrap()),]
        );
        assert_eq!(
            CapabilityId::from_use(&Use {
                event_stream: Some(OneOrMany::One(Name::try_new("test".to_string()).unwrap())),
                ..empty_use()
            },)?,
            vec![CapabilityId::UsedEventStream(
                "/svc/fuchsia.component.EventStream".parse().unwrap()
            ),]
        );
        assert_eq!(
            CapabilityId::from_use(&Use {
                service: Some(OneOrMany::One("a".parse().unwrap())),
                path: Some("/b".parse().unwrap()),
                ..empty_use()
            },)?,
            vec![CapabilityId::UsedService("/b".parse().unwrap())]
        );

        // protocol
        assert_eq!(
            CapabilityId::from_offer_expose(&Offer {
                protocol: Some(OneOrMany::One("a".parse().unwrap())),
                ..empty_offer()
            },)?,
            vec![CapabilityId::Protocol("a".parse().unwrap())]
        );
        assert_eq!(
            CapabilityId::from_offer_expose(&Offer {
                protocol: Some(OneOrMany::Many(vec!["a".parse().unwrap(), "b".parse().unwrap()],)),
                ..empty_offer()
            },)?,
            vec![
                CapabilityId::Protocol("a".parse().unwrap()),
                CapabilityId::Protocol("b".parse().unwrap())
            ]
        );
        assert_eq!(
            CapabilityId::from_use(&Use {
                protocol: Some(OneOrMany::One("a".parse().unwrap())),
                ..empty_use()
            },)?,
            vec![CapabilityId::UsedProtocol("/svc/a".parse().unwrap())]
        );
        assert_eq!(
            CapabilityId::from_use(&Use {
                protocol: Some(OneOrMany::Many(vec!["a".parse().unwrap(), "b".parse().unwrap(),],)),
                ..empty_use()
            },)?,
            vec![
                CapabilityId::UsedProtocol("/svc/a".parse().unwrap()),
                CapabilityId::UsedProtocol("/svc/b".parse().unwrap())
            ]
        );
        assert_eq!(
            CapabilityId::from_use(&Use {
                protocol: Some(OneOrMany::One("a".parse().unwrap())),
                path: Some("/b".parse().unwrap()),
                ..empty_use()
            },)?,
            vec![CapabilityId::UsedProtocol("/b".parse().unwrap())]
        );

        // directory
        assert_eq!(
            CapabilityId::from_offer_expose(&Offer {
                directory: Some(OneOrMany::One("a".parse().unwrap())),
                ..empty_offer()
            },)?,
            vec![CapabilityId::Directory("a".parse().unwrap())]
        );
        assert_eq!(
            CapabilityId::from_offer_expose(&Offer {
                directory: Some(OneOrMany::Many(vec!["a".parse().unwrap(), "b".parse().unwrap()])),
                ..empty_offer()
            },)?,
            vec![
                CapabilityId::Directory("a".parse().unwrap()),
                CapabilityId::Directory("b".parse().unwrap()),
            ]
        );
        assert_eq!(
            CapabilityId::from_use(&Use {
                directory: Some("a".parse().unwrap()),
                path: Some("/b".parse().unwrap()),
                ..empty_use()
            },)?,
            vec![CapabilityId::UsedDirectory("/b".parse().unwrap())]
        );

        // storage
        assert_eq!(
            CapabilityId::from_offer_expose(&Offer {
                storage: Some(OneOrMany::One("cache".parse().unwrap())),
                ..empty_offer()
            },)?,
            vec![CapabilityId::Storage("cache".parse().unwrap())],
        );
        assert_eq!(
            CapabilityId::from_offer_expose(&Offer {
                storage: Some(OneOrMany::Many(vec!["a".parse().unwrap(), "b".parse().unwrap()])),
                ..empty_offer()
            },)?,
            vec![
                CapabilityId::Storage("a".parse().unwrap()),
                CapabilityId::Storage("b".parse().unwrap()),
            ]
        );
        assert_eq!(
            CapabilityId::from_use(&Use {
                storage: Some("a".parse().unwrap()),
                path: Some("/b".parse().unwrap()),
                ..empty_use()
            },)?,
            vec![CapabilityId::UsedStorage("/b".parse().unwrap())]
        );

        // "as" aliasing.
        assert_eq!(
            CapabilityId::from_offer_expose(&Offer {
                service: Some(OneOrMany::One("a".parse().unwrap())),
                r#as: Some("b".parse().unwrap()),
                ..empty_offer()
            },)?,
            vec![CapabilityId::Service("b".parse().unwrap())]
        );

        // Error case.
        assert_matches!(CapabilityId::from_offer_expose(&empty_offer()), Err(_));

        Ok(())
    }

    fn document(contents: serde_json::Value) -> Document {
        serde_json5::from_str::<Document>(&contents.to_string()).unwrap()
    }

    #[test]
    fn test_includes() {
        assert_eq!(document(json!({})).includes(), Vec::<String>::new());
        assert_eq!(document(json!({ "include": []})).includes(), Vec::<String>::new());
        assert_eq!(
            document(json!({ "include": [ "foo.cml", "bar.cml" ]})).includes(),
            vec!["foo.cml", "bar.cml"]
        );
    }

    #[test]
    fn test_merge_same_section() {
        let mut some = document(json!({ "use": [{ "protocol": "foo" }] }));
        let mut other = document(json!({ "use": [{ "protocol": "bar" }] }));
        some.merge_from(&mut other, &Path::new("some/path")).unwrap();
        let uses = some.r#use.as_ref().unwrap();
        assert_eq!(uses.len(), 2);
        assert_eq!(
            uses[0].protocol.as_ref().unwrap(),
            &OneOrMany::One("foo".parse::<Name>().unwrap())
        );
        assert_eq!(
            uses[1].protocol.as_ref().unwrap(),
            &OneOrMany::One("bar".parse::<Name>().unwrap())
        );
    }

    #[test]
    fn test_merge_upgraded_availability() {
        let mut some =
            document(json!({ "use": [{ "protocol": "foo", "availability": "optional" }] }));
        let mut other1 = document(json!({ "use": [{ "protocol": "foo" }] }));
        let mut other2 =
            document(json!({ "use": [{ "protocol": "foo", "availability": "transitional" }] }));
        let mut other3 =
            document(json!({ "use": [{ "protocol": "foo", "availability": "same_as_target" }] }));
        some.merge_from(&mut other1, &Path::new("some/path")).unwrap();
        some.merge_from(&mut other2, &Path::new("some/path")).unwrap();
        some.merge_from(&mut other3, &Path::new("some/path")).unwrap();
        let uses = some.r#use.as_ref().unwrap();
        assert_eq!(uses.len(), 2);
        assert_eq!(
            uses[0].protocol.as_ref().unwrap(),
            &OneOrMany::One("foo".parse::<Name>().unwrap())
        );
        assert!(uses[0].availability.is_none());
        assert_eq!(
            uses[1].protocol.as_ref().unwrap(),
            &OneOrMany::One("foo".parse::<Name>().unwrap())
        );
        assert_eq!(uses[1].availability.as_ref().unwrap(), &Availability::SameAsTarget,);
    }

    #[test]
    fn test_merge_different_sections() {
        let mut some = document(json!({ "use": [{ "protocol": "foo" }] }));
        let mut other = document(json!({ "expose": [{ "protocol": "bar", "from": "self" }] }));
        some.merge_from(&mut other, &Path::new("some/path")).unwrap();
        let uses = some.r#use.as_ref().unwrap();
        let exposes = some.r#expose.as_ref().unwrap();
        assert_eq!(uses.len(), 1);
        assert_eq!(exposes.len(), 1);
        assert_eq!(
            uses[0].protocol.as_ref().unwrap(),
            &OneOrMany::One("foo".parse::<Name>().unwrap())
        );
        assert_eq!(
            exposes[0].protocol.as_ref().unwrap(),
            &OneOrMany::One("bar".parse::<Name>().unwrap())
        );
    }

    #[test]
    fn test_merge_from_other_config() {
        let mut some = document(json!({}));
        let mut other = document(json!({ "config": { "bar": { "type": "bool" } } }));

        some.merge_from(&mut other, &path::Path::new("some/path")).unwrap();
        let expected = document(json!({ "config": { "bar": { "type": "bool" } } }));
        assert_eq!(some.config, expected.config);
    }

    #[test]
    fn test_merge_from_some_config() {
        let mut some = document(json!({ "config": { "bar": { "type": "bool" } } }));
        let mut other = document(json!({}));

        some.merge_from(&mut other, &path::Path::new("some/path")).unwrap();
        let expected = document(json!({ "config": { "bar": { "type": "bool" } } }));
        assert_eq!(some.config, expected.config);
    }

    #[test]
    fn test_merge_from_config() {
        let mut some = document(json!({ "config": { "foo": { "type": "bool" } } }));
        let mut other = document(json!({ "config": { "bar": { "type": "bool" } } }));
        some.merge_from(&mut other, &path::Path::new("some/path")).unwrap();

        assert_eq!(
            some,
            document(json!({
                "config": {
                    "foo": { "type": "bool" },
                    "bar": { "type": "bool" },
                }
            })),
        );
    }

    #[test]
    fn test_merge_from_config_dedupe_identical_fields() {
        let mut some = document(json!({ "config": { "foo": { "type": "bool" } } }));
        let mut other = document(json!({ "config": { "foo": { "type": "bool" } } }));
        some.merge_from(&mut other, &path::Path::new("some/path")).unwrap();

        assert_eq!(some, document(json!({ "config": { "foo": { "type": "bool" } } })));
    }

    #[test]
    fn test_merge_from_config_conflicting_keys() {
        let mut some = document(json!({ "config": { "foo": { "type": "bool" } } }));
        let mut other = document(json!({ "config": { "foo": { "type": "uint8" } } }));

        assert_matches::assert_matches!(
            some.merge_from(&mut other, &path::Path::new("some/path")),
            Err(Error::Validate { err, .. })
                if err == "Found conflicting entry for config key `foo` in `some/path`."
        );
    }

    #[test]
    fn deny_unknown_config_type_fields() {
        let input = json!({ "config": { "foo": { "type": "bool", "unknown": "should error" } } });
        serde_json5::from_str::<Document>(&input.to_string())
            .expect_err("must reject unknown config field attributes");
    }

    #[test]
    fn deny_unknown_config_nested_type_fields() {
        let input = json!({
            "config": {
                "foo": {
                    "type": "vector",
                    "max_count": 10,
                    "element": {
                        "type": "bool",
                        "unknown": "should error"
                    },

                }
            }
        });
        serde_json5::from_str::<Document>(&input.to_string())
            .expect_err("must reject unknown config field attributes");
    }

    #[test]
    fn test_merge_from_program() {
        let mut some = document(json!({ "program": { "binary": "bin/hello_world" } }));
        let mut other = document(json!({ "program": { "runner": "elf" } }));
        some.merge_from(&mut other, &Path::new("some/path")).unwrap();
        let expected =
            document(json!({ "program": { "binary": "bin/hello_world", "runner": "elf" } }));
        assert_eq!(some.program, expected.program);
    }

    #[test]
    fn test_merge_from_program_without_runner() {
        let mut some =
            document(json!({ "program": { "binary": "bin/hello_world", "runner": "elf" } }));
        // fxbug.dev/79951: merging with a document that doesn't have a runner doesn't override the
        // runner that we already have assigned.
        let mut other = document(json!({ "program": {} }));
        some.merge_from(&mut other, &Path::new("some/path")).unwrap();
        let expected =
            document(json!({ "program": { "binary": "bin/hello_world", "runner": "elf" } }));
        assert_eq!(some.program, expected.program);
    }

    #[test]
    fn test_merge_from_program_overlapping_environ() {
        // It's ok to merge `program.environ` by concatenating the arrays together.
        let mut some = document(json!({ "program": { "environ": ["1"] } }));
        let mut other = document(json!({ "program": { "environ": ["2"] } }));
        some.merge_from(&mut other, &Path::new("some/path")).unwrap();
        let expected = document(json!({ "program": { "environ": ["1", "2"] } }));
        assert_eq!(some.program, expected.program);
    }

    #[test]
    fn test_merge_from_program_overlapping_runner() {
        // It's ok to merge `program.runner = "elf"` with `program.runner = "elf"`.
        let mut some =
            document(json!({ "program": { "binary": "bin/hello_world", "runner": "elf" } }));
        let mut other = document(json!({ "program": { "runner": "elf" } }));
        some.merge_from(&mut other, &Path::new("some/path")).unwrap();
        let expected =
            document(json!({ "program": { "binary": "bin/hello_world", "runner": "elf" } }));
        assert_eq!(some.program, expected.program);
    }

    #[test]
    fn test_offer_would_duplicate() {
        let offer = create_offer(
            "fuchsia.logger.LegacyLog",
            OneOrMany::One(OfferFromRef::Parent {}),
            OneOrMany::One(OfferToRef::Named(Name::from_str("something").unwrap())),
        );

        let offer_to_all = create_offer(
            "fuchsia.logger.LogSink",
            OneOrMany::One(OfferFromRef::Parent {}),
            OneOrMany::One(OfferToRef::All),
        );

        // different protocols
        assert!(!offer_to_all_would_duplicate(
            &offer_to_all,
            &offer,
            &Name::from_str("something").unwrap()
        )
        .unwrap());

        let offer = create_offer(
            "fuchsia.logger.LogSink",
            OneOrMany::One(OfferFromRef::Parent {}),
            OneOrMany::One(OfferToRef::Named(Name::from_str("not-something").unwrap())),
        );

        // different targets
        assert!(!offer_to_all_would_duplicate(
            &offer_to_all,
            &offer,
            &Name::from_str("something").unwrap()
        )
        .unwrap());

        let mut offer = create_offer(
            "fuchsia.logger.LogSink",
            OneOrMany::One(OfferFromRef::Parent {}),
            OneOrMany::One(OfferToRef::Named(Name::from_str("something").unwrap())),
        );

        offer.r#as = Some(Name::from_str("FakeLog").unwrap());

        // target has alias
        assert!(!offer_to_all_would_duplicate(
            &offer_to_all,
            &offer,
            &Name::from_str("something").unwrap()
        )
        .unwrap());

        let offer = create_offer(
            "fuchsia.logger.LogSink",
            OneOrMany::One(OfferFromRef::Parent {}),
            OneOrMany::One(OfferToRef::Named(Name::from_str("something").unwrap())),
        );

        assert!(offer_to_all_would_duplicate(
            &offer_to_all,
            &offer,
            &Name::from_str("something").unwrap()
        )
        .unwrap());

        let offer = create_offer(
            "fuchsia.logger.LogSink",
            OneOrMany::One(OfferFromRef::Named(Name::from_str("other").unwrap())),
            OneOrMany::One(OfferToRef::Named(Name::from_str("something").unwrap())),
        );

        assert!(offer_to_all_would_duplicate(
            &offer_to_all,
            &offer,
            &Name::from_str("something").unwrap()
        )
        .is_err());
    }

    #[test_case(
        document(json!({ "program": { "runner": "elf" } })),
        document(json!({ "program": { "runner": "fle" } })),
        "runner"
        ; "when_runner_conflicts"
    )]
    #[test_case(
        document(json!({ "program": { "binary": "bin/hello_world" } })),
        document(json!({ "program": { "binary": "bin/hola_mundo" } })),
        "binary"
        ; "when_binary_conflicts"
    )]
    #[test_case(
        document(json!({ "program": { "args": ["a".to_owned()] } })),
        document(json!({ "program": { "args": ["b".to_owned()] } })),
        "args"
        ; "when_args_conflicts"
    )]
    fn test_merge_from_program_error(mut some: Document, mut other: Document, field: &str) {
        assert_matches::assert_matches!(
            some.merge_from(&mut other, &path::Path::new("some/path")),
            Err(Error::Validate {  err, .. })
                if err == format!("manifest include had a conflicting `program.{}`: some/path", field)
        );
    }

    #[test_case(
        document(json!({ "facets": { "my.key": "my.value" } })),
        document(json!({ "facets": { "other.key": "other.value" } })),
        document(json!({ "facets": { "my.key": "my.value",  "other.key": "other.value" } }))
        ; "two separate keys"
    )]
    #[test_case(
        document(json!({ "facets": { "my.key": "my.value" } })),
        document(json!({ "facets": {} })),
        document(json!({ "facets": { "my.key": "my.value" } }))
        ; "empty other facet"
    )]
    #[test_case(
        document(json!({ "facets": {} })),
        document(json!({ "facets": { "other.key": "other.value" } })),
        document(json!({ "facets": { "other.key": "other.value" } }))
        ; "empty my facet"
    )]
    #[test_case(
        document(json!({ "facets": { "key": { "type": "some_type" } } })),
        document(json!({ "facets": { "key": { "runner": "some_runner"} } })),
        document(json!({ "facets": { "key": { "type": "some_type", "runner": "some_runner" } } }))
        ; "nested facet key"
    )]
    #[test_case(
        document(json!({ "facets": { "key": { "type": "some_type", "nested_key": { "type": "new type" }}}})),
        document(json!({ "facets": { "key": { "nested_key": { "runner": "some_runner" }} } })),
        document(json!({ "facets": { "key": { "type": "some_type", "nested_key": { "runner": "some_runner", "type": "new type" }}}}))
        ; "double nested facet key"
    )]
    #[test_case(
        document(json!({ "facets": { "key": { "array_key": ["value_1", "value_2"] } } })),
        document(json!({ "facets": { "key": { "array_key": ["value_3", "value_4"] } } })),
        document(json!({ "facets": { "key": { "array_key": ["value_1", "value_2", "value_3", "value_4"] } } }))
        ; "merge array values"
    )]
    fn test_merge_from_facets(mut my: Document, mut other: Document, expected: Document) {
        my.merge_from(&mut other, &Path::new("some/path")).unwrap();
        assert_eq!(my.facets, expected.facets);
    }

    #[test_case(
        document(json!({ "facets": { "key": "my.value" }})),
        document(json!({ "facets": { "key": "other.value" }})),
        "facets.key"
        ; "conflict first level keys"
    )]
    #[test_case(
        document(json!({ "facets": { "key":  {"type": "cts" }}})),
        document(json!({ "facets": { "key":  {"type": "system" }}})),
        "facets.key.type"
        ; "conflict second level keys"
    )]
    #[test_case(
        document(json!({ "facets": { "key":  {"type": {"key": "value" }}}})),
        document(json!({ "facets": { "key":  {"type": "system" }}})),
        "facets.key.type"
        ; "incompatible self nested type"
    )]
    #[test_case(
        document(json!({ "facets": { "key":  {"type": "system" }}})),
        document(json!({ "facets": { "key":  {"type":  {"key": "value" }}}})),
        "facets.key.type"
        ; "incompatible other nested type"
    )]
    #[test_case(
        document(json!({ "facets": { "key":  {"type": {"key": "my.value" }}}})),
        document(json!({ "facets": { "key":  {"type":  {"key": "some.value" }}}})),
        "facets.key.type.key"
        ; "conflict third level keys"
    )]
    #[test_case(
        document(json!({ "facets": { "key":  {"type": [ "value_1" ]}}})),
        document(json!({ "facets": { "key":  {"type":  "value_2" }}})),
        "facets.key.type"
        ; "incompatible keys"
    )]
    fn test_merge_from_facet_error(mut my: Document, mut other: Document, field: &str) {
        assert_matches::assert_matches!(
            my.merge_from(&mut other, &path::Path::new("some/path")),
            Err(Error::Validate {  err, .. })
                if err == format!("manifest include had a conflicting `{}`: some/path", field)
        );
    }

    #[test_case("protocol")]
    #[test_case("service")]
    #[test_case("event_stream")]
    fn test_merge_from_duplicate_use_array(typename: &str) {
        let mut my = document(json!({ "use": [{ typename: "a" }]}));
        let mut other = document(json!({ "use": [
            { typename: ["a", "b"], "availability": "optional"}
        ]}));
        let result = document(json!({ "use": [
            { typename: "a" },
            { typename: "b", "availability": "optional" },
        ]}));

        my.merge_from(&mut other, &path::Path::new("some/path")).unwrap();
        assert_eq!(my, result);
    }

    #[test_case("directory")]
    #[test_case("storage")]
    fn test_merge_from_duplicate_use_noarray(typename: &str) {
        let mut my = document(json!({ "use": [{ typename: "a", "path": "/a"}]}));
        let mut other = document(json!({ "use": [
            { typename: "a", "path": "/a", "availability": "optional" },
            { typename: "b", "path": "/b", "availability": "optional" },
        ]}));
        let result = document(json!({ "use": [
            { typename: "a", "path": "/a" },
            { typename: "b", "path": "/b", "availability": "optional" },
        ]}));
        my.merge_from(&mut other, &path::Path::new("some/path")).unwrap();
        assert_eq!(my, result);
    }

    #[test_case("protocol")]
    #[test_case("service")]
    #[test_case("event_stream")]
    fn test_merge_from_duplicate_capabilities_array(typename: &str) {
        let mut my = document(json!({ "capabilities": [{ typename: "a" }]}));
        let mut other = document(json!({ "capabilities": [ { typename: ["a", "b"] } ]}));
        let result = document(json!({ "capabilities": [ { typename: "a" }, { typename: "b" } ]}));

        my.merge_from(&mut other, &path::Path::new("some/path")).unwrap();
        assert_eq!(my, result);
    }

    #[test_case("directory")]
    #[test_case("storage")]
    #[test_case("runner")]
    #[test_case("resolver")]
    fn test_merge_from_duplicate_capabilities_noarray(typename: &str) {
        let mut my = document(json!({ "capabilities": [{ typename: "a", "path": "/a"}]}));
        let mut other = document(json!({ "capabilities": [
            { typename: "a", "path": "/a" },
            { typename: "b", "path": "/b" },
        ]}));
        let result = document(json!({ "capabilities": [
            { typename: "a", "path": "/a" },
            { typename: "b", "path": "/b" },
        ]}));
        my.merge_from(&mut other, &path::Path::new("some/path")).unwrap();
        assert_eq!(my, result);
    }

    #[test_case("protocol")]
    #[test_case("service")]
    #[test_case("event_stream")]
    #[test_case("directory")]
    #[test_case("storage")]
    #[test_case("runner")]
    #[test_case("resolver")]
    fn test_merge_from_duplicate_offers(typename: &str) {
        let mut my = document(json!({ "offer": [{ typename: "a", "from": "self", "to": "#c" }]}));
        let mut other = document(json!({ "offer": [
            { typename: ["a", "b"], "from": "self", "to": "#c", "availability": "optional" }
        ]}));
        let result = document(json!({ "offer": [
            { typename: "a", "from": "self", "to": "#c" },
            { typename: "b", "from": "self", "to": "#c", "availability": "optional" },
        ]}));

        my.merge_from(&mut other, &path::Path::new("some/path")).unwrap();
        assert_eq!(my, result);
    }

    #[test_case("protocol")]
    #[test_case("service")]
    #[test_case("event_stream")]
    #[test_case("directory")]
    #[test_case("runner")]
    #[test_case("resolver")]
    fn test_merge_from_duplicate_exposes(typename: &str) {
        let mut my = document(json!({ "expose": [{ typename: "a", "from": "self" }]}));
        let mut other = document(json!({ "expose": [
            { typename: ["a", "b"], "from": "self" }
        ]}));
        let result = document(json!({ "expose": [
            { typename: "a", "from": "self" },
            { typename: "b", "from": "self" },
        ]}));

        my.merge_from(&mut other, &path::Path::new("some/path")).unwrap();
        assert_eq!(my, result);
    }

    #[test_case(
        document(json!({ "use": [
            { "protocol": "a", "availability": "required" },
            { "protocol": "b", "availability": "optional" },
            { "protocol": "c", "availability": "transitional" },
            { "protocol": "d", "availability": "same_as_target" },
        ]})),
        document(json!({ "use": [
            { "protocol": ["a"], "availability": "required" },
            { "protocol": ["b"], "availability": "optional" },
            { "protocol": ["c"], "availability": "transitional" },
            { "protocol": ["d"], "availability": "same_as_target" },
        ]})),
        document(json!({ "use": [
            { "protocol": "a", "availability": "required" },
            { "protocol": "b", "availability": "optional" },
            { "protocol": "c", "availability": "transitional" },
            { "protocol": "d", "availability": "same_as_target" },
        ]}))
        ; "merge both same"
    )]
    #[test_case(
        document(json!({ "use": [
            { "protocol": "a", "availability": "optional" },
            { "protocol": "b", "availability": "transitional" },
            { "protocol": "c", "availability": "transitional" },
        ]})),
        document(json!({ "use": [
            { "protocol": ["a", "x"], "availability": "required" },
            { "protocol": ["b", "y"], "availability": "optional" },
            { "protocol": ["c", "z"], "availability": "required" },
        ]})),
        document(json!({ "use": [
            { "protocol": ["a", "x"], "availability": "required" },
            { "protocol": ["b", "y"], "availability": "optional" },
            { "protocol": ["c", "z"], "availability": "required" },
        ]}))
        ; "merge with upgrade"
    )]
    #[test_case(
        document(json!({ "use": [
            { "protocol": "a", "availability": "required" },
            { "protocol": "b", "availability": "optional" },
            { "protocol": "c", "availability": "required" },
        ]})),
        document(json!({ "use": [
            { "protocol": ["a", "x"], "availability": "optional" },
            { "protocol": ["b", "y"], "availability": "transitional" },
            { "protocol": ["c", "z"], "availability": "transitional" },
        ]})),
        document(json!({ "use": [
            { "protocol": "a", "availability": "required" },
            { "protocol": "b", "availability": "optional" },
            { "protocol": "c", "availability": "required" },
            { "protocol": "x", "availability": "optional" },
            { "protocol": "y", "availability": "transitional" },
            { "protocol": "z", "availability": "transitional" },
        ]}))
        ; "merge with downgrade"
    )]
    #[test_case(
        document(json!({ "use": [
            { "protocol": "a", "availability": "optional" },
            { "protocol": "b", "availability": "transitional" },
            { "protocol": "c", "availability": "transitional" },
        ]})),
        document(json!({ "use": [
            { "protocol": ["a", "x"], "availability": "same_as_target" },
            { "protocol": ["b", "y"], "availability": "same_as_target" },
            { "protocol": ["c", "z"], "availability": "same_as_target" },
        ]})),
        document(json!({ "use": [
            { "protocol": "a", "availability": "optional" },
            { "protocol": "b", "availability": "transitional" },
            { "protocol": "c", "availability": "transitional" },
            { "protocol": ["a", "x"], "availability": "same_as_target" },
            { "protocol": ["b", "y"], "availability": "same_as_target" },
            { "protocol": ["c", "z"], "availability": "same_as_target" },
        ]}))
        ; "merge with no replacement"
    )]
    #[test_case(
        document(json!({ "use": [
            { "protocol": ["a", "b", "c"], "availability": "optional" },
            { "protocol": "d", "availability": "same_as_target" },
            { "protocol": ["e", "f"] },
        ]})),
        document(json!({ "use": [
            { "protocol": ["c", "e", "g"] },
            { "protocol": ["d", "h"] },
            { "protocol": ["f", "i"], "availability": "transitional" },
        ]})),
        document(json!({ "use": [
            { "protocol": ["a", "b"], "availability": "optional" },
            { "protocol": "d", "availability": "same_as_target" },
            { "protocol": ["e", "f"] },
            { "protocol": ["c", "g"] },
            { "protocol": ["d", "h"] },
            { "protocol": "i", "availability": "transitional" },
        ]}))
        ; "merge multiple"
    )]

    fn test_merge_from_duplicate_capability_availability(
        mut my: Document,
        mut other: Document,
        result: Document,
    ) {
        my.merge_from(&mut other, &path::Path::new("some/path")).unwrap();
        assert_eq!(my, result);
    }

    #[test_case(
        document(json!({ "use": [{ "protocol": ["a", "b"] }]})),
        document(json!({ "use": [{ "protocol": ["c", "d"] }]})),
        document(json!({ "use": [
            { "protocol": ["a", "b"] }, { "protocol": ["c", "d"] }
        ]}))
        ; "merge capabilities with disjoint sets"
    )]
    #[test_case(
        document(json!({ "use": [
            { "protocol": ["a"] },
            { "protocol": "b" },
        ]})),
        document(json!({ "use": [{ "protocol": ["a", "b"] }]})),
        document(json!({ "use": [
            { "protocol": ["a"] }, { "protocol": "b" },
        ]}))
        ; "merge capabilities with equal set"
    )]
    #[test_case(
        document(json!({ "use": [
            { "protocol": ["a", "b"] },
            { "protocol": "c" },
        ]})),
        document(json!({ "use": [{ "protocol": ["a", "b"] }]})),
        document(json!({ "use": [
            { "protocol": ["a", "b"] }, { "protocol": "c" },
        ]}))
        ; "merge capabilities with subset"
    )]
    #[test_case(
        document(json!({ "use": [
            { "protocol": ["a", "b"] },
        ]})),
        document(json!({ "use": [{ "protocol": ["a", "b", "c"] }]})),
        document(json!({ "use": [
            { "protocol": ["a", "b"] },
            { "protocol": "c" },
        ]}))
        ; "merge capabilities with superset"
    )]
    #[test_case(
        document(json!({ "use": [
            { "protocol": ["a", "b"] },
        ]})),
        document(json!({ "use": [{ "protocol": ["b", "c", "d"] }]})),
        document(json!({ "use": [
            { "protocol": ["a", "b"] }, { "protocol": ["c", "d"] }
        ]}))
        ; "merge capabilities with intersection"
    )]
    #[test_case(
        document(json!({ "use": [{ "protocol": ["a", "b"] }]})),
        document(json!({ "use": [
            { "protocol": ["c", "b", "d"] },
            { "protocol": ["e", "d"] },
        ]})),
        document(json!({ "use": [
            {"protocol": ["a", "b"] },
            {"protocol": ["c", "d"] },
            {"protocol": "e" }]}))
        ; "merge capabilities from multiple arrays"
    )]
    #[test_case(
        document(json!({ "use": [{ "protocol": "foo.bar.Baz", "from": "self"}]})),
        document(json!({ "use": [{ "service": "foo.bar.Baz", "from": "self"}]})),
        document(json!({ "use": [
            {"protocol": "foo.bar.Baz", "from": "self"},
            {"service": "foo.bar.Baz", "from": "self"}]}))
        ; "merge capabilities, types don't match"
    )]
    #[test_case(
        document(json!({ "use": [{ "protocol": "foo.bar.Baz", "from": "self"}]})),
        document(json!({ "use": [{ "protocol": "foo.bar.Baz" }]})),
        document(json!({ "use": [
            {"protocol": "foo.bar.Baz", "from": "self"},
            {"protocol": "foo.bar.Baz"}]}))
        ; "merge capabilities, fields don't match"
    )]

    fn test_merge_from_duplicate_capability(
        mut my: Document,
        mut other: Document,
        result: Document,
    ) {
        my.merge_from(&mut other, &path::Path::new("some/path")).unwrap();
        assert_eq!(my, result);
    }

    #[test_case(&Right::Connect; "connect right")]
    #[test_case(&Right::Enumerate; "enumerate right")]
    #[test_case(&Right::Execute; "execute right")]
    #[test_case(&Right::GetAttributes; "getattr right")]
    #[test_case(&Right::ModifyDirectory; "modifydir right")]
    #[test_case(&Right::ReadBytes; "readbytes right")]
    #[test_case(&Right::Traverse; "traverse right")]
    #[test_case(&Right::UpdateAttributes; "updateattrs right")]
    #[test_case(&Right::WriteBytes; "writebytes right")]
    #[test_case(&Right::ReadAlias; "r right")]
    #[test_case(&Right::WriteAlias; "w right")]
    #[test_case(&Right::ExecuteAlias; "x right")]
    #[test_case(&Right::ReadWriteAlias; "rw right")]
    #[test_case(&Right::ReadExecuteAlias; "rx right")]
    #[test_case(&OfferFromRef::Self_; "offer from self")]
    #[test_case(&OfferFromRef::Parent; "offer from parent")]
    #[test_case(&OfferFromRef::Named(Name::try_new("child".to_string()).unwrap()); "offer from named")]
    #[test_case(
        &document(json!({}));
        "empty document"
    )]
    #[test_case(
        &document(json!({ "use": [{ "protocol": "foo.bar.Baz", "from": "self"}]}));
        "use one from self"
    )]
    #[test_case(
        &document(json!({ "use": [{ "protocol": ["foo.bar.Baz", "some.other.Protocol"], "from": "self"}]}));
        "use multiple from self"
    )]
    #[test_case(
        &document(json!({
            "offer": [{ "protocol": "foo.bar.Baz", "from": "self", "to": "#elements"}],
            "collections" :[{"name": "elements", "durability": "transient" }]
        }));
        "offer from self to collection"
    )]
    #[test_case(
        &document(json!({
            "offer": [
                { "service": "foo.bar.Baz", "from": "self", "to": "#elements" },
                { "service": "some.other.Service", "from": "self", "to": "#elements"},
            ],
            "collections":[ {"name": "elements", "durability": "transient"} ]}));
        "service offers"
    )]
    #[test_case(
        &document(json!({ "expose": [{ "protocol": ["foo.bar.Baz", "some.other.Protocol"], "from": "self"}]}));
        "expose protocols from self"
    )]
    #[test_case(
        &document(json!({ "expose": [{ "service": ["foo.bar.Baz", "some.other.Service"], "from": "self"}]}));
        "expose service from self"
    )]
    #[test_case(
        &document(json!({ "capabilities": [{ "protocol": "foo.bar.Baz", "from": "self"}]}));
        "capabilities from self"
    )]
    #[test_case(
        &document(json!({ "facets": { "my.key": "my.value" } }));
        "facets"
    )]
    #[test_case(
        &document(json!({ "program": { "binary": "bin/hello_world", "runner": "elf" } }));
        "elf runner program"
    )]
    fn serialize_roundtrips<T>(val: &T)
    where
        T: serde::de::DeserializeOwned + Serialize + PartialEq + std::fmt::Debug,
    {
        let raw = serde_json::to_string(val).expect("serializing `val` should work");
        let parsed: T =
            serde_json::from_str(&raw).expect("must be able to parse back serialized value");
        assert_eq!(val, &parsed, "parsed value must equal original value");
    }
}
