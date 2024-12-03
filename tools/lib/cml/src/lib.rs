// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A library of common utilities used by `cmc` and related tools.
//! To manually regenerate reference documentation from doc comments in
//! this file, see the instructions at:
//!
//!   tools/lib/reference_doc/macro/derive-reference-doc-tests/src/test_data/README.md

pub mod error;
pub mod features;
pub mod one_or_many;
pub(crate) mod validate;

#[allow(unused)] // A test-only macro is defined outside of a test builds.
pub mod translate;

use crate::error::Error;
use cml_macro::{CheckedVec, OneOrMany, Reference};
use fidl_fuchsia_io as fio;
use indexmap::IndexMap;
use itertools::Itertools;
use json5format::{FormatOptions, PathOption};
use lazy_static::lazy_static;
use maplit::{hashmap, hashset};
use reference_doc::ReferenceDoc;
use serde::{de, ser, Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::Hash;
use std::num::NonZeroU32;
use std::str::FromStr;
use std::{cmp, fmt, path};

pub use cm_types::{
    AllowedOffers, Availability, DeliveryType, DependencyType, Durability, Name, NamespacePath,
    OnTerminate, ParseError, Path, RelativePath, StartupMode, StorageId, Url,
};
use error::Location;

pub use crate::one_or_many::OneOrMany;
pub use crate::translate::{compile, CompileOptions};
pub use crate::validate::ProtocolRequirements;

lazy_static! {
    static ref DEFAULT_EVENT_STREAM_NAME: Name = "EventStream".parse().unwrap();
}

/// Parses a string `buffer` into a [Document]. `file` is used for error reporting.
pub fn parse_one_document(buffer: &String, file: &std::path::Path) -> Result<Document, Error> {
    serde_json5::from_str(&buffer).map_err(|e| {
        let serde_json5::Error::Message { location, msg } = e;
        let location = location.map(|l| Location { line: l.line, column: l.column });
        Error::parse(msg, location, Some(file))
    })
}

/// Parses a string `buffer` into a vector of [Document]. `file` is used for error reporting.
/// Supports JSON encoded as an array of Document JSON objects.
pub fn parse_many_documents(
    buffer: &String,
    file: &std::path::Path,
) -> Result<Vec<Document>, Error> {
    let res: Result<Vec<Document>, _> = serde_json5::from_str(&buffer);
    match res {
        Err(_) => {
            let d = parse_one_document(buffer, file)?;
            Ok(vec![d])
        }
        Ok(docs) => Ok(docs),
    }
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
pub enum CapabilityId<'a> {
    Service(&'a Name),
    Protocol(&'a Name),
    Directory(&'a Name),
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
    // A configuration in a `use` declaration has a target name that matches a config.
    UsedConfiguration(&'a Name),
    UsedRunner(&'a Name),
    Storage(&'a Name),
    Runner(&'a Name),
    Resolver(&'a Name),
    EventStream(&'a Name),
    Dictionary(&'a Name),
    Configuration(&'a Name),
}

/// Generates a `Vec<Name>` -> `Vec<CapabilityId>` conversion function.
macro_rules! capability_ids_from_names {
    ($name:ident, $variant:expr) => {
        fn $name(names: Vec<&'a Name>) -> Vec<Self> {
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

impl<'a> CapabilityId<'a> {
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
            CapabilityId::UsedRunner(_) => "runner",
            CapabilityId::UsedConfiguration(_) => "config",
            CapabilityId::Storage(_) => "storage",
            CapabilityId::Runner(_) => "runner",
            CapabilityId::Resolver(_) => "resolver",
            CapabilityId::EventStream(_) => "event_stream",
            CapabilityId::Dictionary(_) => "dictionary",
            CapabilityId::Configuration(_) => "config",
        }
    }

    /// Return the directory containing the capability, if this capability takes a target path.
    pub fn get_dir_path(&self) -> Option<NamespacePath> {
        match self {
            CapabilityId::UsedService(p)
            | CapabilityId::UsedProtocol(p)
            | CapabilityId::UsedEventStream(p) => Some(p.parent()),
            CapabilityId::UsedDirectory(p) | CapabilityId::UsedStorage(p) => Some(p.clone().into()),
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
    pub fn from_use(use_: &'a Use) -> Result<Vec<Self>, Error> {
        // TODO: Validate that exactly one of these is set.
        let alias = use_.path.as_ref();
        if let Some(n) = use_.service() {
            return Ok(Self::used_services_from(Self::get_one_or_many_svc_paths(
                n,
                alias,
                use_.capability_type().unwrap(),
            )?));
        } else if let Some(n) = use_.protocol() {
            return Ok(Self::used_protocols_from(Self::get_one_or_many_svc_paths(
                n,
                alias,
                use_.capability_type().unwrap(),
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
                "/svc/fuchsia.component.EventStream",
            )?)]);
        } else if let Some(n) = use_.runner() {
            match n {
                OneOrMany::One(name) => {
                    return Ok(vec![CapabilityId::UsedRunner(name)]);
                }
                OneOrMany::Many(_) => {
                    return Err(Error::validate("`use runner` should occur at most once."));
                }
            }
        } else if let Some(_) = use_.config() {
            return match &use_.key {
                None => Err(Error::validate("\"key\" should be present for `use config`.")),
                Some(name) => Ok(vec![CapabilityId::UsedConfiguration(name)]),
            };
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

    pub fn from_capability(capability: &'a Capability) -> Result<Vec<Self>, Error> {
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
                capability.capability_type().unwrap(),
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
                capability.capability_type().unwrap(),
            )?));
        } else if let Some(n) = capability.directory() {
            return Ok(Self::directories_from(Self::get_one_or_many_names(
                n,
                None,
                capability.capability_type().unwrap(),
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
                capability.capability_type().unwrap(),
            )?));
        } else if let Some(n) = capability.runner() {
            return Ok(Self::runners_from(Self::get_one_or_many_names(
                n,
                None,
                capability.capability_type().unwrap(),
            )?));
        } else if let Some(n) = capability.resolver() {
            return Ok(Self::resolvers_from(Self::get_one_or_many_names(
                n,
                None,
                capability.capability_type().unwrap(),
            )?));
        } else if let Some(n) = capability.event_stream() {
            return Ok(Self::event_streams_from(Self::get_one_or_many_names(
                n,
                None,
                capability.capability_type().unwrap(),
            )?));
        } else if let Some(n) = capability.dictionary() {
            return Ok(Self::dictionaries_from(Self::get_one_or_many_names(
                n,
                None,
                capability.capability_type().unwrap(),
            )?));
        } else if let Some(n) = capability.config() {
            return Ok(Self::configurations_from(Self::get_one_or_many_names(
                n,
                None,
                capability.capability_type().unwrap(),
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
    pub fn from_offer_expose<T>(clause: &'a T) -> Result<Vec<Self>, Error>
    where
        T: CapabilityClause + AsClause + fmt::Debug,
    {
        // TODO: Validate that exactly one of these is set.
        let alias = clause.r#as();
        if let Some(n) = clause.service() {
            return Ok(Self::services_from(Self::get_one_or_many_names(
                n,
                alias,
                clause.capability_type().unwrap(),
            )?));
        } else if let Some(n) = clause.protocol() {
            return Ok(Self::protocols_from(Self::get_one_or_many_names(
                n,
                alias,
                clause.capability_type().unwrap(),
            )?));
        } else if let Some(n) = clause.directory() {
            return Ok(Self::directories_from(Self::get_one_or_many_names(
                n,
                alias,
                clause.capability_type().unwrap(),
            )?));
        } else if let Some(n) = clause.storage() {
            return Ok(Self::storages_from(Self::get_one_or_many_names(
                n,
                alias,
                clause.capability_type().unwrap(),
            )?));
        } else if let Some(n) = clause.runner() {
            return Ok(Self::runners_from(Self::get_one_or_many_names(
                n,
                alias,
                clause.capability_type().unwrap(),
            )?));
        } else if let Some(n) = clause.resolver() {
            return Ok(Self::resolvers_from(Self::get_one_or_many_names(
                n,
                alias,
                clause.capability_type().unwrap(),
            )?));
        } else if let Some(event_stream) = clause.event_stream() {
            return Ok(Self::event_streams_from(Self::get_one_or_many_names(
                event_stream,
                alias,
                clause.capability_type().unwrap(),
            )?));
        } else if let Some(n) = clause.dictionary() {
            return Ok(Self::dictionaries_from(Self::get_one_or_many_names(
                n,
                alias,
                clause.capability_type().unwrap(),
            )?));
        } else if let Some(n) = clause.config() {
            return Ok(Self::configurations_from(Self::get_one_or_many_names(
                n,
                alias,
                clause.capability_type().unwrap(),
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
    fn get_one_or_many_names<'b>(
        names: OneOrMany<&'b Name>,
        alias: Option<&'b Name>,
        capability_type: &str,
    ) -> Result<Vec<&'b Name>, Error> {
        let names: Vec<&Name> = names.into_iter().collect();
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
        names: OneOrMany<&Name>,
        alias: Option<&Path>,
        capability_type: &str,
    ) -> Result<Vec<Path>, Error> {
        let names: Vec<_> = names.into_iter().collect();
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
    capability_ids_from_names!(dictionaries_from, CapabilityId::Dictionary);
    capability_ids_from_names!(configurations_from, CapabilityId::Configuration);
    capability_ids_from_paths!(used_services_from, CapabilityId::UsedService);
    capability_ids_from_paths!(used_protocols_from, CapabilityId::UsedProtocol);
}

impl fmt::Display for CapabilityId<'_> {
    /// Return the string ID of this clause.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CapabilityId::Service(n)
            | CapabilityId::Storage(n)
            | CapabilityId::Runner(n)
            | CapabilityId::UsedRunner(n)
            | CapabilityId::Resolver(n)
            | CapabilityId::EventStream(n)
            | CapabilityId::Configuration(n)
            | CapabilityId::UsedConfiguration(n)
            | CapabilityId::Dictionary(n) => write!(f, "{}", n),
            CapabilityId::UsedService(p)
            | CapabilityId::UsedProtocol(p)
            | CapabilityId::UsedDirectory(p)
            | CapabilityId::UsedStorage(p)
            | CapabilityId::UsedEventStream(p) => write!(f, "{}", p),
            CapabilityId::Protocol(p) | CapabilityId::Directory(p) => write!(f, "{}", p),
        }
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
    expected = "one or an array of \"framework\", \"self\", \"#<child-name>\", or a dictionary path",
    inner_type = "ExposeFromRef",
    min_length = 1,
    unique_items = true
)]
pub struct OneOrManyExposeFromRefs;

/// Generates deserializer for `OneOrMany<OfferToRef>`.
#[derive(OneOrMany, Debug, Clone)]
#[one_or_many(
    expected = "one or an array of \"#<child-name>\", \"#<collection-name>\", or \"self/<dictionary>\", with unique elements",
    inner_type = "OfferToRef",
    min_length = 1,
    unique_items = true
)]
pub struct OneOrManyOfferToRefs;

/// Generates deserializer for `OneOrMany<OfferFromRef>`.
#[derive(OneOrMany, Debug, Clone)]
#[one_or_many(
    expected = "one or an array of \"parent\", \"framework\", \"self\", \"#<child-name>\", \"#<collection-name>\", or a dictionary path",
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
    /// A reference to a dictionary. Parsed as a dictionary path.
    Dictionary(&'a DictionaryRef),
    /// A reference to a dictionary defined by this component. Parsed as
    /// `self/<dictionary>`.
    OwnDictionary(&'a Name),
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
            Self::Dictionary(d) => write!(f, "{}", d),
            Self::OwnDictionary(name) => write!(f, "self/{}", name),
        }
    }
}

/// A reference in a `use from`.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Reference)]
#[reference(
    expected = "\"parent\", \"framework\", \"debug\", \"self\", \"#<capability-name>\", \"#<child-name>\", dictionary path, or none"
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
    /// A reference to a capability must be one of the following:
    /// - A dictionary capability.
    /// - A protocol that references a storage capability declared in the same component,
    ///   which will cause the framework to host a fuchsia.sys2.StorageAdmin protocol for the
    ///   component.
    ///
    /// This cannot be used to directly access capabilities that a component itself declares.
    Named(Name),
    /// A reference to this component.
    Self_,
    /// A reference to a dictionary.
    Dictionary(DictionaryRef),
}

/// The scope of an event.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Reference, Ord, PartialOrd)]
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
    /// A reference to a dictionary.
    Dictionary(DictionaryRef),
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
#[reference(
    expected = "\"parent\", \"framework\", \"self\", \"void\", \"#<child-name>\", or a dictionary path"
)]
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
    /// A reference to a dictionary.
    Dictionary(DictionaryRef),
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
#[reference(expected = "\"#<child-name>\", \"#<collection-name>\", or \"self/<dictionary>\"")]
pub enum OfferToRef {
    /// A reference to a child or collection.
    Named(Name),

    /// Syntax sugar that results in the offer decl applying to all children and collections
    All,

    /// A reference to a dictionary defined by this component, the form "self/<dictionary>".
    OwnDictionary(Name),
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

/// A reference to a (possibly nested) dictionary.
#[derive(Debug, PartialEq, Eq, Hash, Clone)]
pub struct DictionaryRef {
    /// Path to the dictionary relative to `root_dictionary`.
    pub path: RelativePath,
    pub root: RootDictionaryRef,
}

impl<'a> From<&'a DictionaryRef> for AnyRef<'a> {
    fn from(r: &'a DictionaryRef) -> Self {
        Self::Dictionary(r)
    }
}

impl FromStr for DictionaryRef {
    type Err = ParseError;

    fn from_str(path: &str) -> Result<Self, ParseError> {
        match path.find('/') {
            Some(n) => {
                let root = path[..n].parse().map_err(|_| ParseError::InvalidValue)?;
                let path = RelativePath::new(&path[n + 1..])?;
                Ok(Self { root, path })
            }
            None => Err(ParseError::InvalidValue),
        }
    }
}

impl fmt::Display for DictionaryRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.root, self.path)
    }
}

impl ser::Serialize for DictionaryRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        format!("{}", self).serialize(serializer)
    }
}

const DICTIONARY_REF_EXPECT_STR: &str = "a path to a dictionary no more \
    than 4095 characters in length";

impl<'de> de::Deserialize<'de> for DictionaryRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = DictionaryRef;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(DICTIONARY_REF_EXPECT_STR)
            }

            fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                s.parse().map_err(|err| match err {
                    ParseError::InvalidValue => {
                        E::invalid_value(de::Unexpected::Str(s), &DICTIONARY_REF_EXPECT_STR)
                    }
                    ParseError::TooLong | ParseError::Empty => {
                        E::invalid_length(s.len(), &DICTIONARY_REF_EXPECT_STR)
                    }
                    e => {
                        panic!("unexpected parse error: {:?}", e);
                    }
                })
            }
        }

        deserializer.deserialize_string(Visitor)
    }
}

/// A reference to a root dictionary.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Reference)]
#[reference(expected = "\"parent\", \"self\", \"#<child-name>\"")]
pub enum RootDictionaryRef {
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
/// The `.cml` file is compiled into a FIDL wire format (`.cm`) file.
///
/// ## String types
///
/// ### Names {#names}
///
/// Both capabilities and a component's children are named. A name string may
/// consist of one or more of the following characters: `A-Z`, `a-z`, `0-9`,
/// `_`, `.`, `-`. It must not exceed 255 characters in length and may not start
/// with `.` or `-`.
///
/// ### Paths {#paths}
///
/// Paths are sequences of [names](#names) delimited by the `/` character. A path
/// must not exceed 4095 characters in length. Throughout the document,
///
/// - Relative paths cannot start with the `/` character.
/// - Namespace and outgoing directory paths must start with the `/` character.
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
/// [doc-dictionaries]: /reference/fidl/fuchsia.component.decl#Dictionary
/// [doc-directory]: /docs/concepts/components/v2/capabilities/directory.md
/// [doc-storage]: /docs/concepts/components/v2/capabilities/storage.md
/// [doc-resolvers]: /docs/concepts/components/v2/capabilities/resolver.md
/// [doc-runners]: /docs/concepts/components/v2/capabilities/runner.md
/// [doc-event]: /docs/concepts/components/v2/capabilities/event.md
/// [doc-service]: /docs/concepts/components/v2/capabilities/service.md
/// [doc-directory-rights]: /docs/concepts/components/v2/capabilities/directory.md#directory-capability-rights
///
/// ## Top-level keys {#document}
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
    /// fx cmc include {{ "<var>" }}cml_file{{ "</var>" }} --includeroot $FUCHSIA_DIR --includepath $FUCHSIA_DIR/sdk/lib
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
    /// [`use`]: #use
    /// [`offer`]: #offer
    /// [`expose`]: #expose
    /// [`capabilities`]: #capabilities
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<String>>,

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
    /// [doc-runners]: /docs/concepts/components/v2/capabilities/runner.md
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
    /// # Capability fields
    ///
    /// This supports the following capability keys. Exactly one of these must be set:
    ///
    /// - `protocol`: (_optional `string or array of strings`_)
    /// - `service`: (_optional `string or array of strings`_)
    /// - `directory`: (_optional `string`_)
    /// - `storage`: (_optional `string`_)
    /// - `runner`: (_optional `string`_)
    /// - `resolver`: (_optional `string`_)
    /// - `event_stream`: (_optional `string or array of strings`_)
    /// - `dictionary`: (_optional `string`_)
    /// - `config`: (_optional `string`_)
    ///
    /// # Additional fields
    ///
    /// This supports the following additional fields:
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
    /// # Capability fields
    ///
    /// This supports the following capability keys. Exactly one of these must be set:
    ///
    /// - `service`: (_optional `string or array of strings`_)
    /// - `directory`: (_optional `string`_)
    /// - `protocol`: (_optional `string or array of strings`_)
    /// - `dictionary`: (_optional `string`_)
    /// - `storage`: (_optional `string`_)
    /// - `event_stream`: (_optional `string or array of strings`_)
    /// - `runner`: (_optional `string`_)
    /// - `config`: (_optional `string`_)
    ///
    /// # Additional fields
    ///
    /// This supports the following additional fields:
    /// [glossary.namespace]: /docs/glossary/README.md#namespace
    #[reference_doc(recurse)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#use: Option<Vec<Use>>,

    /// Declares the capabilities that are made available to the parent component or to the
    /// framework. It is valid to `expose` from `self` or from a child component.
    ///
    /// # Capability fields
    ///
    /// This supports the following capability keys. Exactly one of these must be set:
    ///
    /// - `service`: (_optional `string or array of strings`_)
    /// - `protocol`: (_optional `string or array of strings`_)
    /// - `directory`: (_optional `string`_)
    /// - `runner`: (_optional `string`_)
    /// - `resolver`: (_optional `string`_)
    /// - `dictionary`: (_optional `string`_)
    /// - `config`: (_optional `string`_)
    ///
    /// # Additional fields
    ///
    /// This supports the following additional fields:
    #[reference_doc(recurse)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expose: Option<Vec<Expose>>,

    /// Declares the capabilities that are made available to a [child component][doc-children]
    /// instance or a [child collection][doc-collections].
    ///
    /// # Capability fields
    ///
    /// This supports the following capability keys. Exactly one of these must be set:
    ///
    /// - `protocol`: (_optional `string or array of strings`_)
    /// - `service`: (_optional `string or array of strings`_)
    /// - `directory`: (_optional `string`_)
    /// - `storage`: (_optional `string`_)
    /// - `runner`: (_optional `string`_)
    /// - `resolver`: (_optional `string`_)
    /// - `event_stream`: (_optional `string or array of strings`_)
    /// - `dictionary`: (_optional `string`_)
    /// - `config`: (_optional `string`_)
    ///
    /// # Additional fields
    ///
    /// This supports the following additional fields:
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
    /// Fields are resolved from a component's package by default. To be able to change the values
    /// at runtime a `mutability` specifier is required.
    ///
    /// Example:
    ///
    /// ```json5
    /// config: {
    ///     verbose: {
    ///         type: "bool",
    ///         mutability: [ "parent" ],
    ///     },
    /// },
    /// ```
    ///
    /// Currently `"parent"` is the only mutability specifier supported.
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

impl<T> Canonicalize for Vec<T>
where
    T: Canonicalize + CapabilityClause + PathClause,
{
    fn canonicalize(&mut self) {
        // Collapse like-entries into one. Like entries are those that are equal in all fields
        // but their capability names. Accomplish this by collecting all the names into a vector
        // keyed by an instance of T with its names removed.
        let mut to_merge: Vec<(T, Vec<Name>)> = vec![];
        let mut to_keep: Vec<T> = vec![];
        self.iter().for_each(|c| {
            // Any entry with a `path` set cannot be merged with another.
            if !c.are_many_names_allowed() || c.path().is_some() {
                to_keep.push(c.clone());
                return;
            }
            let mut names = c.names().into_iter().cloned().collect();
            let mut copy = c.clone();
            copy.set_names(vec![Name::from_str("a").unwrap()]); // The name here is arbitrary.
            let r = to_merge.iter().position(|(t, _)| t == &copy);
            match r {
                Some(i) => to_merge[i].1.append(&mut names),
                None => to_merge.push((copy, names)),
            };
        });
        let mut merged = to_merge
            .into_iter()
            .map(|(mut t, names)| {
                t.set_names(names);
                t
            })
            .collect::<Vec<_>>();
        to_keep.append(&mut merged);
        *self = to_keep;

        self.iter_mut().for_each(|c| c.canonicalize());
        self.sort_by(|a, b| {
            // Sort by capability type, then by the name of the first entry for
            // that type.
            let a_type = a.capability_type().unwrap();
            let b_type = b.capability_type().unwrap();
            a_type.cmp(b_type).then_with(|| {
                let a_names = a.names();
                let b_names = b.names();
                let a_first_name = a_names.first().unwrap();
                let b_first_name = b_names.first().unwrap();
                a_first_name.cmp(b_first_name)
            })
        });
    }
}

/// Merges `us` into `other` according to the rules documented for [`include`].
/// [`include`]: #include
fn merge_from_capability_field<T: CapabilityClause>(
    us: &mut Option<Vec<T>>,
    other: &mut Option<Vec<T>>,
) -> Result<(), Error> {
    // Empty entries are an error, and merging removes empty entries so we first need to check
    // for them.
    for entry in us.iter().flatten().chain(other.iter().flatten()) {
        if entry.names().is_empty() {
            return Err(Error::Validate {
                err: format!("{}: Missing type name: {:#?}", entry.decl_type(), entry),
                filename: None,
            });
        }
    }

    if let Some(all_ours) = us.as_mut() {
        if let Some(all_theirs) = other.take() {
            for mut theirs in all_theirs {
                for ours in &mut *all_ours {
                    compute_diff(ours, &mut theirs);
                }
                all_ours.push(theirs);
            }
        }
        // Post-filter step: remove empty entries.
        all_ours.retain(|ours| !ours.names().is_empty())
    } else if let Some(theirs) = other.take() {
        us.replace(theirs);
    }
    Ok(())
}

/// Merges `us` into `other` according to the rules documented for [`include`].
/// [`include`]: #include
fn merge_from_other_field<T: std::cmp::PartialEq>(
    us: &mut Option<Vec<T>>,
    other: &mut Option<Vec<T>>,
) {
    if let Some(ref mut ours) = us {
        if let Some(theirs) = other.take() {
            // Add their elements, ignoring dupes with ours
            for t in theirs {
                if !ours.contains(&t) {
                    ours.push(t);
                }
            }
        }
    } else if let Some(theirs) = other.take() {
        us.replace(theirs);
    }
}

/// Subtracts the capabilities in `ours` from `theirs` if the declarations match in their type and
/// other fields, resulting in the removal of duplicates between `ours` and `theirs`. Stores the
/// result in `theirs`.
///
/// Inexact matches on `availability` are allowed if there is a partial order between them. The
/// stronger availability is chosen.
fn compute_diff<T: CapabilityClause>(ours: &mut T, theirs: &mut T) {
    // Return early if one is empty.
    if ours.names().is_empty() || theirs.names().is_empty() {
        return;
    }

    // Return early if the types don't match.
    if ours.capability_type().unwrap() != theirs.capability_type().unwrap() {
        return;
    }

    // Check if the non-capability fields match before proceeding.
    let mut ours_partial = ours.clone();
    let mut theirs_partial = theirs.clone();
    for e in [&mut ours_partial, &mut theirs_partial] {
        e.set_names(Vec::new());
        // Availability is allowed to differ (see merge algorithm below)
        e.set_availability(None);
    }
    if ours_partial != theirs_partial {
        // The fields other than `availability` do not match, nothing to remove.
        return;
    }

    // Compare the availabilities.
    let Some(avail_cmp) = ours
        .availability()
        .unwrap_or_default()
        .partial_cmp(&theirs.availability().unwrap_or_default())
    else {
        // The availabilities are incompatible (no partial order).
        return;
    };

    let mut our_names: Vec<_> = ours.names().into_iter().cloned().collect();
    let mut their_names: Vec<_> = theirs.names().into_iter().cloned().collect();

    let mut our_entries_to_remove = HashSet::new();
    let mut their_entries_to_remove = HashSet::new();
    for e in &their_names {
        if !our_names.contains(e) {
            // Not a duplicate, so keep.
            continue;
        }
        match avail_cmp {
            cmp::Ordering::Less => {
                // Their availability is stronger, meaning theirs should take
                // priority. Keep `e` in theirs, and remove it from ours.
                our_entries_to_remove.insert(e.clone());
            }
            cmp::Ordering::Greater => {
                // Our availability is stronger, meaning ours should take
                // priority. Remove `e` from theirs.
                their_entries_to_remove.insert(e.clone());
            }
            cmp::Ordering::Equal => {
                // The availabilities are equal, so `e` is a duplicate.
                their_entries_to_remove.insert(e.clone());
            }
        }
    }
    our_names.retain(|e| !our_entries_to_remove.contains(e));
    their_names.retain(|e| !their_entries_to_remove.contains(e));

    ours.set_names(our_names);
    theirs.set_names(their_names);
}

impl Document {
    pub fn merge_from(
        &mut self,
        other: &mut Document,
        include_path: &path::Path,
    ) -> Result<(), Error> {
        // Flatten the mergable fields that may contain a
        // list of capabilities in one clause.
        merge_from_capability_field(&mut self.r#use, &mut other.r#use)?;
        merge_from_capability_field(&mut self.expose, &mut other.expose)?;
        merge_from_capability_field(&mut self.offer, &mut other.offer)?;
        merge_from_capability_field(&mut self.capabilities, &mut other.capabilities)?;
        merge_from_other_field(&mut self.include, &mut other.include);
        merge_from_other_field(&mut self.children, &mut other.children);
        merge_from_other_field(&mut self.collections, &mut other.collections);
        self.merge_environment(other, include_path)?;
        self.merge_program(other, include_path)?;
        self.merge_facets(other, include_path)?;
        self.merge_config(other, include_path)?;

        Ok(())
    }

    pub fn canonicalize(&mut self) {
        // Don't sort `include` - the order there matters.
        if let Some(children) = &mut self.children {
            children.sort_by(|a, b| a.name.cmp(&b.name));
        }
        if let Some(collections) = &mut self.collections {
            collections.sort_by(|a, b| a.name.cmp(&b.name));
        }
        if let Some(environments) = &mut self.environments {
            environments.sort_by(|a, b| a.name.cmp(&b.name));
        }
        if let Some(capabilities) = &mut self.capabilities {
            capabilities.canonicalize();
        }
        if let Some(offers) = &mut self.offer {
            offers.canonicalize();
        }
        if let Some(expose) = &mut self.expose {
            expose.canonicalize();
        }
        if let Some(r#use) = &mut self.r#use {
            r#use.canonicalize();
        }
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
            Some(vec!["environ", "features"]),
        )
    }

    fn merge_environment(
        &mut self,
        other: &mut Document,
        _include_path: &path::Path,
    ) -> Result<(), Error> {
        if let None = other.environments {
            return Ok(());
        }
        if let None = self.environments {
            self.environments = Some(vec![]);
        }

        let my_environments = self.environments.as_mut().unwrap();
        let other_environments = other.environments.as_mut().unwrap();
        my_environments.sort_by(|x, y| x.name.cmp(&y.name));
        other_environments.sort_by(|x, y| x.name.cmp(&y.name));

        let all_environments =
            my_environments.into_iter().merge_by(other_environments, |x, y| x.name <= y.name);
        let groups = all_environments.group_by(|e| e.name.clone());

        let mut merged_environments = vec![];
        for (name, group) in groups.into_iter() {
            let mut merged_environment = Environment {
                name: name.clone(),
                extends: None,
                runners: None,
                resolvers: None,
                debug: None,
                stop_timeout_ms: None,
            };
            for e in group {
                merged_environment.merge_from(e)?;
            }
            merged_environments.push(merged_environment);
        }

        self.environments = Some(merged_environments);
        Ok(())
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

    pub fn all_storage_with_sources<'a>(&'a self) -> HashMap<&'a Name, &'a CapabilityFromRef> {
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
                c.iter().filter_map(|c| c.service.as_ref()).map(|p| p.iter()).flatten().collect()
            })
            .unwrap_or_else(|| vec![])
    }

    pub fn all_protocol_names(&self) -> Vec<&Name> {
        self.capabilities
            .as_ref()
            .map(|c| {
                c.iter().filter_map(|c| c.protocol.as_ref()).map(|p| p.iter()).flatten().collect()
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

    pub fn all_dictionary_names(&self) -> Vec<&Name> {
        if let Some(capabilities) = self.capabilities.as_ref() {
            capabilities.iter().filter_map(|c| c.dictionary.as_ref()).collect()
        } else {
            vec![]
        }
    }

    pub fn all_dictionaries<'a>(&'a self) -> HashMap<&'a Name, &'a Capability> {
        if let Some(capabilities) = self.capabilities.as_ref() {
            capabilities
                .iter()
                .filter_map(|c| match c.dictionary.as_ref() {
                    Some(s) => Some((s, c)),
                    _ => None,
                })
                .collect()
        } else {
            HashMap::new()
        }
    }

    pub fn all_config_names(&self) -> Vec<&Name> {
        self.capabilities
            .as_ref()
            .map(|c| c.iter().filter_map(|c| c.config.as_ref()).collect())
            .unwrap_or_else(|| vec![])
    }

    pub fn all_environment_names(&self) -> Vec<&Name> {
        self.environments
            .as_ref()
            .map(|c| c.iter().map(|s| &s.name).collect())
            .unwrap_or_else(|| vec![])
    }

    pub fn all_capability_names(&self) -> HashSet<&Name> {
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
///         extends: "realm",
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
    /// should terminate, before forcibly killing it. This field is required if the environment
    /// extends from `none`.
    #[serde(rename = "__stop_timeout_ms")]
    #[reference_doc(json_type = "number", rename = "__stop_timeout_ms")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_timeout_ms: Option<StopTimeoutMs>,
}

impl Environment {
    pub fn merge_from(&mut self, other: &mut Self) -> Result<(), Error> {
        if self.extends.is_none() {
            self.extends = other.extends.take();
        } else if other.extends.is_some() && other.extends != self.extends {
            return Err(Error::validate(
                "cannot merge `environments` that declare conflicting `extends`",
            ));
        }

        if self.stop_timeout_ms.is_none() {
            self.stop_timeout_ms = other.stop_timeout_ms;
        } else if other.stop_timeout_ms.is_some() && other.stop_timeout_ms != self.stop_timeout_ms {
            return Err(Error::validate(
                "cannot merge `environments` that declare conflicting `stop_timeout_ms`",
            ));
        }

        // Perform naive vector concatenation and rely on later validation to ensure
        // no conflicting entries.
        match &mut self.runners {
            Some(r) => {
                if let Some(o) = &mut other.runners {
                    r.append(o);
                }
            }
            None => self.runners = other.runners.take(),
        }

        match &mut self.resolvers {
            Some(r) => {
                if let Some(o) = &mut other.resolvers {
                    r.append(o);
                }
            }
            None => self.resolvers = other.resolvers.take(),
        }

        match &mut self.debug {
            Some(r) => {
                if let Some(o) = &mut other.debug {
                    r.append(o);
                }
            }
            None => self.debug = other.debug.take(),
        }
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigType {
    Bool,
    Uint8,
    Uint16,
    Uint32,
    Uint64,
    Int8,
    Int16,
    Int32,
    Int64,
    String,
    Vector,
}

impl From<&cm_rust::ConfigValueType> for ConfigType {
    fn from(value: &cm_rust::ConfigValueType) -> Self {
        match value {
            cm_rust::ConfigValueType::Bool => ConfigType::Bool,
            cm_rust::ConfigValueType::Uint8 => ConfigType::Uint8,
            cm_rust::ConfigValueType::Int8 => ConfigType::Int8,
            cm_rust::ConfigValueType::Uint16 => ConfigType::Uint16,
            cm_rust::ConfigValueType::Int16 => ConfigType::Int16,
            cm_rust::ConfigValueType::Uint32 => ConfigType::Uint32,
            cm_rust::ConfigValueType::Int32 => ConfigType::Int32,
            cm_rust::ConfigValueType::Uint64 => ConfigType::Uint64,
            cm_rust::ConfigValueType::Int64 => ConfigType::Int64,
            cm_rust::ConfigValueType::String { .. } => ConfigType::String,
            cm_rust::ConfigValueType::Vector { .. } => ConfigType::Vector,
        }
    }
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

impl From<ConfigValueType> for cm_rust::ConfigValueType {
    fn from(value: ConfigValueType) -> Self {
        match value {
            ConfigValueType::Bool { .. } => cm_rust::ConfigValueType::Bool,
            ConfigValueType::Uint8 { .. } => cm_rust::ConfigValueType::Uint8,
            ConfigValueType::Uint16 { .. } => cm_rust::ConfigValueType::Uint16,
            ConfigValueType::Uint32 { .. } => cm_rust::ConfigValueType::Uint32,
            ConfigValueType::Uint64 { .. } => cm_rust::ConfigValueType::Uint64,
            ConfigValueType::Int8 { .. } => cm_rust::ConfigValueType::Int8,
            ConfigValueType::Int16 { .. } => cm_rust::ConfigValueType::Int16,
            ConfigValueType::Int32 { .. } => cm_rust::ConfigValueType::Int32,
            ConfigValueType::Int64 { .. } => cm_rust::ConfigValueType::Int64,
            ConfigValueType::String { max_size, .. } => {
                cm_rust::ConfigValueType::String { max_size: max_size.into() }
            }
            ConfigValueType::Vector { max_count, element, .. } => {
                cm_rust::ConfigValueType::Vector {
                    max_count: max_count.into(),
                    nested_type: element.into(),
                }
            }
        }
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

impl From<ConfigNestedValueType> for cm_rust::ConfigNestedValueType {
    fn from(value: ConfigNestedValueType) -> Self {
        match value {
            ConfigNestedValueType::Bool {} => cm_rust::ConfigNestedValueType::Bool,
            ConfigNestedValueType::Uint8 {} => cm_rust::ConfigNestedValueType::Uint8,
            ConfigNestedValueType::Uint16 {} => cm_rust::ConfigNestedValueType::Uint16,
            ConfigNestedValueType::Uint32 {} => cm_rust::ConfigNestedValueType::Uint32,
            ConfigNestedValueType::Uint64 {} => cm_rust::ConfigNestedValueType::Uint64,
            ConfigNestedValueType::Int8 {} => cm_rust::ConfigNestedValueType::Int8,
            ConfigNestedValueType::Int16 {} => cm_rust::ConfigNestedValueType::Int16,
            ConfigNestedValueType::Int32 {} => cm_rust::ConfigNestedValueType::Int32,
            ConfigNestedValueType::Int64 {} => cm_rust::ConfigNestedValueType::Int64,
            ConfigNestedValueType::String { max_size } => {
                cm_rust::ConfigNestedValueType::String { max_size: max_size.into() }
            }
        }
    }
}

impl TryFrom<&cm_rust::ConfigNestedValueType> for ConfigNestedValueType {
    type Error = ();
    fn try_from(nested: &cm_rust::ConfigNestedValueType) -> Result<Self, ()> {
        Ok(match nested {
            cm_rust::ConfigNestedValueType::Bool => ConfigNestedValueType::Bool {},
            cm_rust::ConfigNestedValueType::Uint8 => ConfigNestedValueType::Uint8 {},
            cm_rust::ConfigNestedValueType::Int8 => ConfigNestedValueType::Int8 {},
            cm_rust::ConfigNestedValueType::Uint16 => ConfigNestedValueType::Uint16 {},
            cm_rust::ConfigNestedValueType::Int16 => ConfigNestedValueType::Int16 {},
            cm_rust::ConfigNestedValueType::Uint32 => ConfigNestedValueType::Uint32 {},
            cm_rust::ConfigNestedValueType::Int32 => ConfigNestedValueType::Int32 {},
            cm_rust::ConfigNestedValueType::Uint64 => ConfigNestedValueType::Uint64 {},
            cm_rust::ConfigNestedValueType::Int64 => ConfigNestedValueType::Int64 {},
            cm_rust::ConfigNestedValueType::String { max_size } => {
                ConfigNestedValueType::String { max_size: NonZeroU32::new(*max_size).ok_or(())? }
            }
        })
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
    #[serde(skip_serializing_if = "Option::is_none")]
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

#[derive(Deserialize, Debug, PartialEq, Clone, ReferenceDoc, Serialize, Default)]
#[serde(deny_unknown_fields)]
#[reference_doc(fields_as = "list")]
pub struct Capability {
    /// The [name](#name) for this service capability. Specifying `path` is valid
    /// only when this value is a string.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub service: Option<OneOrMany<Name>>,

    /// The [name](#name) for this protocol capability. Specifying `path` is valid
    /// only when this value is a string.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub protocol: Option<OneOrMany<Name>>,

    /// The [name](#name) for this directory capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub directory: Option<Name>,

    /// The [name](#name) for this storage capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub storage: Option<Name>,

    /// The [name](#name) for this runner capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub runner: Option<Name>,

    /// The [name](#name) for this resolver capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub resolver: Option<Name>,

    /// The [name](#name) for this event_stream capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub event_stream: Option<OneOrMany<Name>>,

    /// The [name](#name) for this dictionary capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub dictionary: Option<Name>,

    /// The [name](#name) for this configuration capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub config: Option<Name>,

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
    ///
    /// For `dictionary`, this is optional. If provided, it is a path to a
    /// `fuchsia.component.sandbox/DictionaryRouter` served by the program which should return a
    /// `fuchsia.component.sandbox/DictionaryRef`, by which the program may dynamically provide
    /// a dictionary from itself. If this is set for `dictionary`, `offer` to this dictionary
    /// is not allowed.
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

    /// (`storage` only) The identifier used to isolated storage for a component, one of:
    /// - `static_instance_id`: The instance ID in the component ID index is used
    ///     as the key for a component's storage. Components which are not listed in
    ///     the component ID index will not be able to use this storage capability.
    /// - `static_instance_id_or_moniker`: If the component is listed in the
    ///     component ID index, the instance ID is used as the key for a component's
    ///     storage. Otherwise, the component's moniker from the storage
    ///     capability is used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage_id: Option<StorageId>,

    /// (`configuration` only) The type of configuration, one of:
    /// - `bool`: Boolean type.
    /// - `uint8`: Unsigned 8 bit type.
    /// - `uint16`: Unsigned 16 bit type.
    /// - `uint32`: Unsigned 32 bit type.
    /// - `uint64`: Unsigned 64 bit type.
    /// - `int8`: Signed 8 bit type.
    /// - `int16`: Signed 16 bit type.
    /// - `int32`: Signed 32 bit type.
    /// - `int64`: Signed 64 bit type.
    /// - `string`: ASCII string type.
    /// - `vector`: Vector type. See `element` for the type of the element within the vector.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    #[reference_doc(rename = "type")]
    pub config_type: Option<ConfigType>,

    /// (`configuration` only) Only supported if this configuration `type` is 'string'.
    /// This is the max size of the string.
    #[serde(rename = "max_size", skip_serializing_if = "Option::is_none")]
    #[reference_doc(rename = "max_size")]
    pub config_max_size: Option<NonZeroU32>,

    /// (`configuration` only) Only supported if this configuration `type` is 'vector'.
    /// This is the max number of elements in the vector.
    #[serde(rename = "max_count", skip_serializing_if = "Option::is_none")]
    #[reference_doc(rename = "max_count")]
    pub config_max_count: Option<NonZeroU32>,

    /// (`configuration` only) Only supported if this configuration `type` is 'vector'.
    /// This is the type of the elements in the configuration vector.
    ///
    /// Example (simple type):
    ///
    /// ```json5
    /// { type: "uint8" }
    /// ```
    ///
    /// Example (string type):
    ///
    /// ```json5
    /// {
    ///   type: "string",
    ///   max_size: 100,
    /// }
    /// ```
    #[serde(rename = "element", skip_serializing_if = "Option::is_none")]
    #[reference_doc(rename = "element", json_type = "object")]
    pub config_element_type: Option<ConfigNestedValueType>,

    /// (`configuration` only) The value of the configuration.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,

    /// (`protocol` only) Specifies when the framework will open the protocol
    /// from this component's outgoing directory when someone requests the
    /// capability. Allowed values are:
    ///
    /// - `eager`: (default) the framework will open the capability as soon as
    ///   some consumer component requests it.
    /// - `on_readable`: the framework will open the capability when the server
    ///   endpoint pipelined in a connection request becomes readable.
    ///
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<DeliveryType>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
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
            "a non-empty `runner` string property no more than 255 characters in length \
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
                        Name::new(r.clone()).map_err(|e| match e {
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
///     {
///         runner: "own_test_runner".
///         from: "#test_runner",
///     },
/// ],
/// ```
#[derive(Deserialize, Debug, Default, PartialEq, Clone, ReferenceDoc, Serialize)]
#[serde(deny_unknown_fields)]
#[reference_doc(fields_as = "list", top_level_doc_after_fields)]
pub struct Use {
    /// When using a service capability, the [name](#name) of a [service capability][doc-service].
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub service: Option<OneOrMany<Name>>,

    /// When using a protocol capability, the [name](#name) of a [protocol capability][doc-protocol].
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub protocol: Option<OneOrMany<Name>>,

    /// When using a directory capability, the [name](#name) of a [directory capability][doc-directory].
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub directory: Option<Name>,

    /// When using a storage capability, the [name](#name) of a [storage capability][doc-storage].
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub storage: Option<Name>,

    /// When using an event stream capability, the [name](#name) of an [event stream capability][doc-event].
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub event_stream: Option<OneOrMany<Name>>,

    /// When using a runner capability, the [name](#name) of a [runner capability][doc-runners].
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub runner: Option<Name>,

    /// When using a configuration capability, the [name](#name) of a [configuration capability][doc-configuration].
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub config: Option<Name>,

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
    ///
    /// [fidl-environment-decl]: /reference/fidl/fuchsia.component.decl#Environment
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<UseFromRef>,

    /// The path at which to install the capability in the component's namespace. For protocols,
    /// defaults to `/svc/${protocol}`.  Required for `directory` and `storage`. This property is
    /// disallowed for declarations with arrays of capability names and for runner capabilities.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<Path>,

    /// (`directory` only) the maximum [directory rights][doc-directory-rights] to apply to
    /// the directory in the component's namespace.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(json_type = "array of string")]
    pub rights: Option<Rights>,

    /// (`directory` only) A subdirectory within the directory capability to provide in the
    /// component's namespace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdir: Option<RelativePath>,

    /// (`event_stream` only) When defined the event stream will contain events about only the
    /// components defined in the scope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<OneOrMany<EventScope>>,

    /// (`event_stream` only) Capability requested event streams require specifying a filter
    /// referring to the protocol to which the events in the event stream apply. The content of the
    /// filter will be an object mapping from "name" to the "protocol name".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<Map<String, Value>>,

    /// The type of dependency between the source and
    /// this component, one of:
    /// - `strong`: a strong dependency, which is used to determine shutdown
    ///     ordering. Component manager is guaranteed to stop the target before the
    ///     source. This is the default.
    /// - `weak`: a weak dependency, which is ignored during shutdown. When component manager
    ///     stops the parent realm, the source may stop before the clients. Clients of weak
    ///     dependencies must be able to handle these dependencies becoming unavailable.
    /// This property is disallowed for runner capabilities, which are always a `strong` dependency.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependency: Option<DependencyType>,

    /// The expectations around this capability's availability. One
    /// of:
    /// - `required` (default): a required dependency, the component is unable to perform its
    ///     work without this capability.
    /// - `optional`: an optional dependency, the component will be able to function without this
    ///     capability (although if the capability is unavailable some functionality may be
    ///     disabled).
    /// - `transitional`: the source may omit the route completely without even having to route
    ///     from `void`. Used for soft transitions that introduce new capabilities.
    /// This property is disallowed for runner capabilities, which are always `required`.
    ///
    /// For more information, see the
    /// [availability](/docs/concepts/components/v2/capabilities/availability.md) documentation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub availability: Option<Availability>,

    /// (`config` only) The configuration key in the component's `config` block that this capability
    /// will set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<Name>,

    /// (`config` only) The type of configuration, one of:
    /// - `bool`: Boolean type.
    /// - `uint8`: Unsigned 8 bit type.
    /// - `uint16`: Unsigned 16 bit type.
    /// - `uint32`: Unsigned 32 bit type.
    /// - `uint64`: Unsigned 64 bit type.
    /// - `int8`: Signed 8 bit type.
    /// - `int16`: Signed 16 bit type.
    /// - `int32`: Signed 32 bit type.
    /// - `int64`: Signed 64 bit type.
    /// - `string`: ASCII string type.
    /// - `vector`: Vector type. See `element` for the type of the element within the vector
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    #[reference_doc(rename = "type")]
    pub config_type: Option<ConfigType>,

    /// (`configuration` only) Only supported if this configuration `type` is 'string'.
    /// This is the max size of the string.
    #[serde(rename = "max_size", skip_serializing_if = "Option::is_none")]
    #[reference_doc(rename = "max_size")]
    pub config_max_size: Option<NonZeroU32>,

    /// (`configuration` only) Only supported if this configuration `type` is 'vector'.
    /// This is the max number of elements in the vector.
    #[serde(rename = "max_count", skip_serializing_if = "Option::is_none")]
    #[reference_doc(rename = "max_count")]
    pub config_max_count: Option<NonZeroU32>,

    /// (`configuration` only) Only supported if this configuration `type` is 'vector'.
    /// This is the type of the elements in the configuration vector.
    ///
    /// Example (simple type):
    ///
    /// ```json5
    /// { type: "uint8" }
    /// ```
    ///
    /// Example (string type):
    ///
    /// ```json5
    /// {
    ///   type: "string",
    ///   max_size: 100,
    /// }
    /// ```
    #[serde(rename = "element", skip_serializing_if = "Option::is_none")]
    #[reference_doc(rename = "element", json_type = "object")]
    pub config_element_type: Option<ConfigNestedValueType>,

    /// (`configuration` only) The default value of this configuration.
    /// Default values are used if the capability is optional and routed from `void`.
    /// This is only supported if `availability` is not `required``.
    #[serde(rename = "default", skip_serializing_if = "Option::is_none")]
    #[reference_doc(rename = "default")]
    pub config_default: Option<serde_json::Value>,
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
    #[reference_doc(skip = true)]
    pub service: Option<OneOrMany<Name>>,

    /// When routing a protocol, the [name](#name) of a [protocol capability][doc-protocol].
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub protocol: Option<OneOrMany<Name>>,

    /// When routing a directory, the [name](#name) of a [directory capability][doc-directory].
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub directory: Option<OneOrMany<Name>>,

    /// When routing a runner, the [name](#name) of a [runner capability][doc-runners].
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub runner: Option<OneOrMany<Name>>,

    /// When routing a resolver, the [name](#name) of a [resolver capability][doc-resolvers].
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub resolver: Option<OneOrMany<Name>>,

    /// When routing a dictionary, the [name](#name) of a [dictionary capability][doc-dictionaries].
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub dictionary: Option<OneOrMany<Name>>,

    /// When routing a config, the [name](#name) of a configuration capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(skip = true)]
    pub config: Option<OneOrMany<Name>>,

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
    #[reference_doc(json_type = "array of string")]
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
    ///
    /// For more information, see the
    /// [availability](/docs/concepts/components/v2/capabilities/availability.md) documentation.
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
            config: None,
            runner: None,
            resolver: None,
            dictionary: None,
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
///         dependency: "weak",
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

    /// When routing a dictionary, the [name](#name) of a [dictionary capability][doc-dictionaries].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dictionary: Option<OneOrMany<Name>>,

    /// When routing a config, the [name](#name) of a configuration capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<OneOrMany<Name>>,

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

    /// Capability target(s). One of:
    /// - `#<target-name>` or \[`#name1`, ...\]: A [reference](#references) to a child or collection,
    ///   or an array of references.
    /// - `all`: Short-hand for an `offer` clause containing all child [references](#references).
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
    /// - `weak`: a weak dependency, which is ignored during
    ///     shutdown. When component manager stops the parent realm, the source may
    ///     stop before the clients. Clients of weak dependencies must be able to
    ///     handle these dependencies becoming unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependency: Option<DependencyType>,

    /// (`directory` only) the maximum [directory rights][doc-directory-rights] to apply to
    /// the offered directory capability.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[reference_doc(json_type = "array of string")]
    pub rights: Option<Rights>,

    /// (`directory` only) the relative path of a subdirectory within the source directory
    /// capability to route.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdir: Option<RelativePath>,

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
    ///
    /// For more information, see the
    /// [availability](/docs/concepts/components/v2/capabilities/availability.md) documentation.
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

    /// Allow child names up to 1024 characters long instead of the usual 255 character limit.
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

pub trait CapabilityClause: Clone + PartialEq + std::fmt::Debug {
    fn service(&self) -> Option<OneOrMany<&Name>>;
    fn protocol(&self) -> Option<OneOrMany<&Name>>;
    fn directory(&self) -> Option<OneOrMany<&Name>>;
    fn storage(&self) -> Option<OneOrMany<&Name>>;
    fn runner(&self) -> Option<OneOrMany<&Name>>;
    fn resolver(&self) -> Option<OneOrMany<&Name>>;
    fn event_stream(&self) -> Option<OneOrMany<&Name>>;
    fn dictionary(&self) -> Option<OneOrMany<&Name>>;
    fn config(&self) -> Option<OneOrMany<&Name>>;
    fn set_service(&mut self, o: Option<OneOrMany<Name>>);
    fn set_protocol(&mut self, o: Option<OneOrMany<Name>>);
    fn set_directory(&mut self, o: Option<OneOrMany<Name>>);
    fn set_storage(&mut self, o: Option<OneOrMany<Name>>);
    fn set_runner(&mut self, o: Option<OneOrMany<Name>>);
    fn set_resolver(&mut self, o: Option<OneOrMany<Name>>);
    fn set_event_stream(&mut self, o: Option<OneOrMany<Name>>);
    fn set_dictionary(&mut self, o: Option<OneOrMany<Name>>);
    fn set_config(&mut self, o: Option<OneOrMany<Name>>);

    fn availability(&self) -> Option<Availability>;
    fn set_availability(&mut self, a: Option<Availability>);

    /// Returns the name of the capability for display purposes.
    /// If `service()` returns `Some`, the capability name must be "service", etc.
    ///
    /// Returns an error if the capability name is not set, or if there is more than one.
    fn capability_type(&self) -> Result<&'static str, Error> {
        let mut types = Vec::new();
        if self.service().is_some() {
            types.push("service");
        }
        if self.protocol().is_some() {
            types.push("protocol");
        }
        if self.directory().is_some() {
            types.push("directory");
        }
        if self.storage().is_some() {
            types.push("storage");
        }
        if self.event_stream().is_some() {
            types.push("event_stream");
        }
        if self.runner().is_some() {
            types.push("runner");
        }
        if self.config().is_some() {
            types.push("config");
        }
        if self.resolver().is_some() {
            types.push("resolver");
        }
        if self.dictionary().is_some() {
            types.push("dictionary");
        }
        match types.len() {
            0 => {
                let supported_keywords = self
                    .supported()
                    .into_iter()
                    .map(|k| format!("\"{}\"", k))
                    .collect::<Vec<_>>()
                    .join(", ");
                Err(Error::validate(format!(
                    "`{}` declaration is missing a capability keyword, one of: {}",
                    self.decl_type(),
                    supported_keywords,
                )))
            }
            1 => Ok(types[0]),
            _ => Err(Error::validate(format!(
                "{} declaration has multiple capability types defined: {:?}",
                self.decl_type(),
                types
            ))),
        }
    }

    /// Returns true if this capability type allows the ::Many variant of OneOrMany.
    fn are_many_names_allowed(&self) -> bool;

    fn decl_type(&self) -> &'static str;
    fn supported(&self) -> &[&'static str];

    /// Returns the names of the capabilities in this clause.
    /// If `protocol()` returns `Some(OneOrMany::Many(vec!["a", "b"]))`, this returns!["a", "b"].
    fn names(&self) -> Vec<&Name> {
        let res = vec![
            self.service(),
            self.protocol(),
            self.directory(),
            self.storage(),
            self.runner(),
            self.config(),
            self.resolver(),
            self.event_stream(),
            self.dictionary(),
        ];
        res.into_iter()
            .map(|o| o.map(|o| o.into_iter().collect::<Vec<&Name>>()).unwrap_or(vec![]))
            .flatten()
            .collect()
    }

    fn set_names(&mut self, names: Vec<Name>) {
        let names = match names.len() {
            0 => None,
            1 => Some(OneOrMany::One(names.first().unwrap().clone())),
            _ => Some(OneOrMany::Many(names)),
        };

        let cap_type = self.capability_type().unwrap();
        if cap_type == "protocol" {
            self.set_protocol(names);
        } else if cap_type == "service" {
            self.set_service(names);
        } else if cap_type == "directory" {
            self.set_directory(names);
        } else if cap_type == "storage" {
            self.set_storage(names);
        } else if cap_type == "runner" {
            self.set_runner(names);
        } else if cap_type == "resolver" {
            self.set_resolver(names);
        } else if cap_type == "event_stream" {
            self.set_event_stream(names);
        } else if cap_type == "dictionary" {
            self.set_dictionary(names);
        } else if cap_type == "config" {
            self.set_config(names);
        } else {
            panic!("Unknown capability type {}", cap_type);
        }
    }
}

trait Canonicalize {
    fn canonicalize(&mut self);
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

impl Canonicalize for Capability {
    fn canonicalize(&mut self) {
        // Sort the names of the capabilities. Only capabilities with OneOrMany values are included here.
        if let Some(service) = &mut self.service {
            service.canonicalize()
        } else if let Some(protocol) = &mut self.protocol {
            protocol.canonicalize()
        } else if let Some(event_stream) = &mut self.event_stream {
            event_stream.canonicalize()
        }
    }
}

fn option_one_or_many_as_ref<T>(o: &Option<OneOrMany<T>>) -> Option<OneOrMany<&T>> {
    o.as_ref().map(|o| o.as_ref())
}

impl CapabilityClause for Capability {
    fn service(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.service)
    }
    fn protocol(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.protocol)
    }
    fn directory(&self) -> Option<OneOrMany<&Name>> {
        self.directory.as_ref().map(|n| OneOrMany::One(n))
    }
    fn storage(&self) -> Option<OneOrMany<&Name>> {
        self.storage.as_ref().map(|n| OneOrMany::One(n))
    }
    fn runner(&self) -> Option<OneOrMany<&Name>> {
        self.runner.as_ref().map(|n| OneOrMany::One(n))
    }
    fn resolver(&self) -> Option<OneOrMany<&Name>> {
        self.resolver.as_ref().map(|n| OneOrMany::One(n))
    }
    fn event_stream(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.event_stream)
    }
    fn dictionary(&self) -> Option<OneOrMany<&Name>> {
        self.dictionary.as_ref().map(|n| OneOrMany::One(n))
    }
    fn config(&self) -> Option<OneOrMany<&Name>> {
        self.config.as_ref().map(|n| OneOrMany::One(n))
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
    fn set_dictionary(&mut self, o: Option<OneOrMany<Name>>) {
        self.dictionary = always_one(o);
    }

    fn set_config(&mut self, o: Option<OneOrMany<Name>>) {
        self.config = always_one(o);
    }

    fn availability(&self) -> Option<Availability> {
        None
    }
    fn set_availability(&mut self, _a: Option<Availability>) {}

    fn decl_type(&self) -> &'static str {
        "capability"
    }
    fn supported(&self) -> &[&'static str] {
        &[
            "service",
            "protocol",
            "directory",
            "storage",
            "runner",
            "resolver",
            "event_stream",
            "dictionary",
            "config",
        ]
    }
    fn are_many_names_allowed(&self) -> bool {
        ["service", "protocol", "event_stream"].contains(&self.capability_type().unwrap())
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
    fn service(&self) -> Option<OneOrMany<&Name>> {
        None
    }
    fn protocol(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.protocol)
    }
    fn directory(&self) -> Option<OneOrMany<&Name>> {
        None
    }
    fn storage(&self) -> Option<OneOrMany<&Name>> {
        None
    }
    fn runner(&self) -> Option<OneOrMany<&Name>> {
        None
    }
    fn resolver(&self) -> Option<OneOrMany<&Name>> {
        None
    }
    fn event_stream(&self) -> Option<OneOrMany<&Name>> {
        None
    }
    fn dictionary(&self) -> Option<OneOrMany<&Name>> {
        None
    }
    fn config(&self) -> Option<OneOrMany<&Name>> {
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
    fn set_dictionary(&mut self, _o: Option<OneOrMany<Name>>) {}
    fn set_config(&mut self, _o: Option<OneOrMany<Name>>) {}

    fn availability(&self) -> Option<Availability> {
        None
    }
    fn set_availability(&mut self, _a: Option<Availability>) {}

    fn decl_type(&self) -> &'static str {
        "debug"
    }
    fn supported(&self) -> &[&'static str] {
        &["service", "protocol"]
    }
    fn are_many_names_allowed(&self) -> bool {
        ["protocol"].contains(&self.capability_type().unwrap())
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

impl Canonicalize for Use {
    fn canonicalize(&mut self) {
        // Sort the names of the capabilities. Only capabilities with OneOrMany values are included here.
        if let Some(service) = &mut self.service {
            service.canonicalize();
        } else if let Some(protocol) = &mut self.protocol {
            protocol.canonicalize();
        } else if let Some(event_stream) = &mut self.event_stream {
            event_stream.canonicalize();
            if let Some(scope) = &mut self.scope {
                scope.canonicalize();
            }
        }
    }
}

impl CapabilityClause for Use {
    fn service(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.service)
    }
    fn protocol(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.protocol)
    }
    fn directory(&self) -> Option<OneOrMany<&Name>> {
        self.directory.as_ref().map(|n| OneOrMany::One(n))
    }
    fn storage(&self) -> Option<OneOrMany<&Name>> {
        self.storage.as_ref().map(|n| OneOrMany::One(n))
    }
    fn runner(&self) -> Option<OneOrMany<&Name>> {
        self.runner.as_ref().map(|n| OneOrMany::One(n))
    }
    fn resolver(&self) -> Option<OneOrMany<&Name>> {
        None
    }
    fn event_stream(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.event_stream)
    }
    fn dictionary(&self) -> Option<OneOrMany<&Name>> {
        None
    }
    fn config(&self) -> Option<OneOrMany<&Name>> {
        self.config.as_ref().map(|n| OneOrMany::One(n))
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
    fn set_dictionary(&mut self, _o: Option<OneOrMany<Name>>) {}
    fn set_config(&mut self, o: Option<OneOrMany<Name>>) {
        self.config = always_one(o);
    }

    fn availability(&self) -> Option<Availability> {
        self.availability
    }
    fn set_availability(&mut self, a: Option<Availability>) {
        self.availability = a;
    }

    fn decl_type(&self) -> &'static str {
        "use"
    }
    fn supported(&self) -> &[&'static str] {
        &["service", "protocol", "directory", "storage", "event_stream", "runner", "config"]
    }
    fn are_many_names_allowed(&self) -> bool {
        ["service", "protocol", "event_stream"].contains(&self.capability_type().unwrap())
    }
}

impl FilterClause for Use {
    fn filter(&self) -> Option<&Map<String, Value>> {
        self.filter.as_ref()
    }
}

impl PathClause for Use {
    fn path(&self) -> Option<&Path> {
        self.path.as_ref()
    }
}

impl FromClause for Use {
    fn from_(&self) -> OneOrMany<AnyRef<'_>> {
        let one = match &self.from {
            Some(from) => AnyRef::from(from),
            // Default for `use`.
            None => AnyRef::Parent,
        };
        OneOrMany::One(one)
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

impl Canonicalize for Expose {
    fn canonicalize(&mut self) {
        // Sort the names of the capabilities. Only capabilities with OneOrMany values are included here.
        if let Some(service) = &mut self.service {
            service.canonicalize();
        } else if let Some(protocol) = &mut self.protocol {
            protocol.canonicalize();
        } else if let Some(directory) = &mut self.directory {
            directory.canonicalize();
        } else if let Some(runner) = &mut self.runner {
            runner.canonicalize();
        } else if let Some(resolver) = &mut self.resolver {
            resolver.canonicalize();
        } else if let Some(event_stream) = &mut self.event_stream {
            event_stream.canonicalize();
            if let Some(scope) = &mut self.scope {
                scope.canonicalize();
            }
        }
        // TODO(https://fxbug.dev/300500098): canonicalize dictionaries
    }
}

impl CapabilityClause for Expose {
    fn service(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.service)
    }
    fn protocol(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.protocol)
    }
    fn directory(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.directory)
    }
    fn storage(&self) -> Option<OneOrMany<&Name>> {
        None
    }
    fn runner(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.runner)
    }
    fn resolver(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.resolver)
    }
    fn event_stream(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.event_stream)
    }
    fn dictionary(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.dictionary)
    }
    fn config(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.config)
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
    fn set_dictionary(&mut self, o: Option<OneOrMany<Name>>) {
        self.dictionary = o;
    }
    fn set_config(&mut self, o: Option<OneOrMany<Name>>) {
        self.config = o;
    }

    fn availability(&self) -> Option<Availability> {
        None
    }
    fn set_availability(&mut self, _a: Option<Availability>) {}

    fn decl_type(&self) -> &'static str {
        "expose"
    }
    fn supported(&self) -> &[&'static str] {
        &[
            "service",
            "protocol",
            "directory",
            "runner",
            "resolver",
            "event_stream",
            "dictionary",
            "config",
        ]
    }
    fn are_many_names_allowed(&self) -> bool {
        [
            "service",
            "protocol",
            "directory",
            "runner",
            "resolver",
            "event_stream",
            "dictionary",
            "config",
        ]
        .contains(&self.capability_type().unwrap())
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

impl Canonicalize for Offer {
    fn canonicalize(&mut self) {
        // Sort the names of the capabilities. Only capabilities with OneOrMany values are included here.
        if let Some(service) = &mut self.service {
            service.canonicalize();
        } else if let Some(protocol) = &mut self.protocol {
            protocol.canonicalize();
        } else if let Some(directory) = &mut self.directory {
            directory.canonicalize();
        } else if let Some(runner) = &mut self.runner {
            runner.canonicalize();
        } else if let Some(resolver) = &mut self.resolver {
            resolver.canonicalize();
        } else if let Some(storage) = &mut self.storage {
            storage.canonicalize();
        } else if let Some(event_stream) = &mut self.event_stream {
            event_stream.canonicalize();
            if let Some(scope) = &mut self.scope {
                scope.canonicalize();
            }
        }
    }
}

impl CapabilityClause for Offer {
    fn service(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.service)
    }
    fn protocol(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.protocol)
    }
    fn directory(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.directory)
    }
    fn storage(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.storage)
    }
    fn runner(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.runner)
    }
    fn resolver(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.resolver)
    }
    fn event_stream(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.event_stream)
    }
    fn dictionary(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.dictionary)
    }
    fn config(&self) -> Option<OneOrMany<&Name>> {
        option_one_or_many_as_ref(&self.config)
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
    fn set_dictionary(&mut self, o: Option<OneOrMany<Name>>) {
        self.dictionary = o;
    }
    fn set_config(&mut self, o: Option<OneOrMany<Name>>) {
        self.config = o
    }

    fn availability(&self) -> Option<Availability> {
        self.availability
    }
    fn set_availability(&mut self, a: Option<Availability>) {
        self.availability = a;
    }

    fn decl_type(&self) -> &'static str {
        "offer"
    }
    fn supported(&self) -> &[&'static str] {
        &[
            "service",
            "protocol",
            "directory",
            "storage",
            "runner",
            "resolver",
            "event_stream",
            "config",
        ]
    }
    fn are_many_names_allowed(&self) -> bool {
        [
            "service",
            "protocol",
            "directory",
            "storage",
            "runner",
            "resolver",
            "event_stream",
            "config",
        ]
        .contains(&self.capability_type().unwrap())
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

pub fn alias_or_name<'a>(alias: Option<&'a Name>, name: &'a Name) -> &'a Name {
    alias.unwrap_or(name)
}

pub fn alias_or_path<'a>(alias: Option<&'a Path>, path: &'a Path) -> &'a Path {
    alias.unwrap_or(path)
}

pub fn format_cml(buffer: &str, file: Option<&std::path::Path>) -> Result<Vec<u8>, Error> {
    let general_order = PathOption::PropertyNameOrder(vec![
        "name",
        "url",
        "startup",
        "environment",
        "config",
        "dictionary",
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

    json5format::format(buffer, file.map(|f| f.to_string_lossy().to_string()), Some(options))
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
    // Only protocols and dictionaries may be offered to all
    assert!(offer_to_all.protocol.is_some() || offer_to_all.dictionary.is_some());

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

impl Offer {
    /// Creates a new empty offer. This offer just has the `from` and `to` fields set, so to make
    /// it useful it needs at least the capability name set in the necesssary attribute.
    pub fn empty(from: OneOrMany<OfferFromRef>, to: OneOrMany<OfferToRef>) -> Offer {
        Self {
            protocol: None,
            from,
            to,
            r#as: None,
            service: None,
            directory: None,
            config: None,
            runner: None,
            resolver: None,
            storage: None,
            dictionary: None,
            dependency: None,
            rights: None,
            subdir: None,
            event_stream: None,
            scope: None,
            availability: None,
            source_availability: None,
        }
    }
}

#[cfg(test)]
pub fn create_offer(
    protocol_name: &str,
    from: OneOrMany<OfferFromRef>,
    to: OneOrMany<OfferToRef>,
) -> Offer {
    Offer {
        protocol: Some(OneOrMany::One(Name::from_str(protocol_name).unwrap())),
        ..Offer::empty(from, to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use difference::Changeset;
    use serde_json::{json, to_string_pretty, to_value};
    use std::path::Path;
    use test_case::test_case;

    macro_rules! assert_json_eq {
        ($a:expr, $e:expr) => {{
            if $a != $e {
                let expected = to_string_pretty(&$e).unwrap();
                let actual = to_string_pretty(&$a).unwrap();
                assert_eq!(
                    $a,
                    $e,
                    "JSON actual != expected. Diffs:\n\n{}",
                    Changeset::new(&actual, &expected, "\n")
                );
            }
        }};
    }

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

    fn json_value_from_str(json: &str, filename: &Path) -> Result<Value, Error> {
        serde_json::from_str(json).map_err(|e| {
            Error::parse(
                format!("Couldn't read input as JSON: {}", e),
                Some(Location { line: e.line(), column: e.column() }),
                Some(filename),
            )
        })
    }

    fn parse_as_ref(input: &str) -> Result<OfferFromRef, Error> {
        serde_json::from_value::<OfferFromRef>(json_value_from_str(input, &Path::new("test.cml"))?)
            .map_err(|e| Error::parse(format!("{}", e), None, None))
    }

    #[test]
    fn test_deserialize_ref() -> Result<(), Error> {
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
            dictionary: None,
            config: None,
            from: OneOrMany::One(OfferFromRef::Self_),
            to: OneOrMany::Many(vec![]),
            r#as: None,
            rights: None,
            subdir: None,
            dependency: None,
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
            config: None,
            key: None,
            from: None,
            path: None,
            rights: None,
            subdir: None,
            event_stream: None,
            runner: None,
            filter: None,
            dependency: None,
            availability: None,
            config_element_type: None,
            config_max_count: None,
            config_max_size: None,
            config_type: None,
            config_default: None,
        }
    }

    #[test]
    fn test_capability_id() -> Result<(), Error> {
        // service
        let a: Name = "a".parse().unwrap();
        let b: Name = "b".parse().unwrap();
        assert_eq!(
            CapabilityId::from_offer_expose(&Offer {
                service: Some(OneOrMany::One(a.clone())),
                ..empty_offer()
            },)?,
            vec![CapabilityId::Service(&a)]
        );
        assert_eq!(
            CapabilityId::from_offer_expose(&Offer {
                service: Some(OneOrMany::Many(vec![a.clone(), b.clone()],)),
                ..empty_offer()
            },)?,
            vec![CapabilityId::Service(&a), CapabilityId::Service(&b)]
        );
        assert_eq!(
            CapabilityId::from_use(&Use {
                service: Some(OneOrMany::One(a.clone())),
                ..empty_use()
            },)?,
            vec![CapabilityId::UsedService("/svc/a".parse().unwrap())]
        );
        assert_eq!(
            CapabilityId::from_use(&Use {
                service: Some(OneOrMany::Many(vec![a.clone(), b.clone(),],)),
                ..empty_use()
            },)?,
            vec![
                CapabilityId::UsedService("/svc/a".parse().unwrap()),
                CapabilityId::UsedService("/svc/b".parse().unwrap())
            ]
        );
        assert_eq!(
            CapabilityId::from_use(&Use {
                event_stream: Some(OneOrMany::One(Name::new("test".to_string()).unwrap())),
                path: Some(cm_types::Path::new("/svc/myevent".to_string()).unwrap()),
                ..empty_use()
            },)?,
            vec![CapabilityId::UsedEventStream("/svc/myevent".parse().unwrap()),]
        );
        assert_eq!(
            CapabilityId::from_use(&Use {
                event_stream: Some(OneOrMany::One(Name::new("test".to_string()).unwrap())),
                ..empty_use()
            },)?,
            vec![CapabilityId::UsedEventStream(
                "/svc/fuchsia.component.EventStream".parse().unwrap()
            ),]
        );
        assert_eq!(
            CapabilityId::from_use(&Use {
                service: Some(OneOrMany::One(a.clone())),
                path: Some("/b".parse().unwrap()),
                ..empty_use()
            },)?,
            vec![CapabilityId::UsedService("/b".parse().unwrap())]
        );

        // protocol
        assert_eq!(
            CapabilityId::from_offer_expose(&Offer {
                protocol: Some(OneOrMany::One(a.clone())),
                ..empty_offer()
            },)?,
            vec![CapabilityId::Protocol(&a)]
        );
        assert_eq!(
            CapabilityId::from_offer_expose(&Offer {
                protocol: Some(OneOrMany::Many(vec![a.clone(), b.clone()],)),
                ..empty_offer()
            },)?,
            vec![CapabilityId::Protocol(&a), CapabilityId::Protocol(&b)]
        );
        assert_eq!(
            CapabilityId::from_use(&Use {
                protocol: Some(OneOrMany::One(a.clone())),
                ..empty_use()
            },)?,
            vec![CapabilityId::UsedProtocol("/svc/a".parse().unwrap())]
        );
        assert_eq!(
            CapabilityId::from_use(&Use {
                protocol: Some(OneOrMany::Many(vec![a.clone(), b.clone(),],)),
                ..empty_use()
            },)?,
            vec![
                CapabilityId::UsedProtocol("/svc/a".parse().unwrap()),
                CapabilityId::UsedProtocol("/svc/b".parse().unwrap())
            ]
        );
        assert_eq!(
            CapabilityId::from_use(&Use {
                protocol: Some(OneOrMany::One(a.clone())),
                path: Some("/b".parse().unwrap()),
                ..empty_use()
            },)?,
            vec![CapabilityId::UsedProtocol("/b".parse().unwrap())]
        );

        // directory
        assert_eq!(
            CapabilityId::from_offer_expose(&Offer {
                directory: Some(OneOrMany::One(a.clone())),
                ..empty_offer()
            },)?,
            vec![CapabilityId::Directory(&a)]
        );
        assert_eq!(
            CapabilityId::from_offer_expose(&Offer {
                directory: Some(OneOrMany::Many(vec![a.clone(), b.clone()])),
                ..empty_offer()
            },)?,
            vec![CapabilityId::Directory(&a), CapabilityId::Directory(&b),]
        );
        assert_eq!(
            CapabilityId::from_use(&Use {
                directory: Some(a.clone()),
                path: Some("/b".parse().unwrap()),
                ..empty_use()
            },)?,
            vec![CapabilityId::UsedDirectory("/b".parse().unwrap())]
        );

        // storage
        assert_eq!(
            CapabilityId::from_offer_expose(&Offer {
                storage: Some(OneOrMany::One(a.clone())),
                ..empty_offer()
            },)?,
            vec![CapabilityId::Storage(&a)]
        );
        assert_eq!(
            CapabilityId::from_offer_expose(&Offer {
                storage: Some(OneOrMany::Many(vec![a.clone(), b.clone()])),
                ..empty_offer()
            },)?,
            vec![CapabilityId::Storage(&a), CapabilityId::Storage(&b),]
        );
        assert_eq!(
            CapabilityId::from_use(&Use {
                storage: Some(a.clone()),
                path: Some("/b".parse().unwrap()),
                ..empty_use()
            },)?,
            vec![CapabilityId::UsedStorage("/b".parse().unwrap())]
        );

        // runner
        assert_eq!(
            CapabilityId::from_use(&Use { runner: Some("elf".parse().unwrap()), ..empty_use() },)?,
            vec![CapabilityId::UsedRunner(&"elf".parse().unwrap())]
        );

        // "as" aliasing.
        assert_eq!(
            CapabilityId::from_offer_expose(&Offer {
                service: Some(OneOrMany::One(a.clone())),
                r#as: Some(b.clone()),
                ..empty_offer()
            },)?,
            vec![CapabilityId::Service(&b)]
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
        let exposes = some.expose.as_ref().unwrap();
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
    fn test_merge_environments() {
        let mut some = document(json!({ "environments": [
            {
                "name": "one",
                "extends": "realm",
            },
            {
                "name": "two",
                "extends": "none",
                "runners": [
                    {
                        "runner": "r1",
                        "from": "#c1",
                    },
                    {
                        "runner": "r2",
                        "from": "#c2",
                    },
                ],
                "resolvers": [
                    {
                        "resolver": "res1",
                        "from": "#c1",
                        "scheme": "foo",
                    },
                ],
                "debug": [
                    {
                        "protocol": "baz",
                        "from": "#c2"
                    }
                ]
            },
        ]}));
        let mut other = document(json!({ "environments": [
            {
                "name": "two",
                "__stop_timeout_ms": 100,
                "runners": [
                    {
                        "runner": "r3",
                        "from": "#c3",
                    },
                ],
                "resolvers": [
                    {
                        "resolver": "res2",
                        "from": "#c1",
                        "scheme": "bar",
                    },
                ],
                "debug": [
                    {
                        "protocol": "faz",
                        "from": "#c2"
                    }
                ]
            },
            {
                "name": "three",
                "__stop_timeout_ms": 1000,
            },
        ]}));
        some.merge_from(&mut other, &Path::new("some/path")).unwrap();
        assert_eq!(
            to_value(some).unwrap(),
            json!({"environments": [
                {
                    "name": "one",
                    "extends": "realm",
                },
                {
                    "name": "three",
                    "__stop_timeout_ms": 1000,
                },
                {
                    "name": "two",
                    "extends": "none",
                    "__stop_timeout_ms": 100,
                    "runners": [
                        {
                            "runner": "r1",
                            "from": "#c1",
                        },
                        {
                            "runner": "r2",
                            "from": "#c2",
                        },
                        {
                            "runner": "r3",
                            "from": "#c3",
                        },
                    ],
                    "resolvers": [
                        {
                            "resolver": "res1",
                            "from": "#c1",
                            "scheme": "foo",
                        },
                        {
                            "resolver": "res2",
                            "from": "#c1",
                            "scheme": "bar",
                        },
                    ],
                    "debug": [
                        {
                            "protocol": "baz",
                            "from": "#c2"
                        },
                        {
                            "protocol": "faz",
                            "from": "#c2"
                        }
                    ]
                },
            ]})
        );
    }

    #[test]
    fn test_merge_environments_errors() {
        {
            let mut some = document(json!({"environments": [{"name": "one", "extends": "realm"}]}));
            let mut other = document(json!({"environments": [{"name": "one", "extends": "none"}]}));
            assert!(some.merge_from(&mut other, &Path::new("some/path")).is_err());
        }
        {
            let mut some =
                document(json!({"environments": [{"name": "one", "__stop_timeout_ms": 10}]}));
            let mut other =
                document(json!({"environments": [{"name": "one", "__stop_timeout_ms": 20}]}));
            assert!(some.merge_from(&mut other, &Path::new("some/path")).is_err());
        }

        // It's ok if the values match.
        {
            let mut some = document(json!({"environments": [{"name": "one", "extends": "realm"}]}));
            let mut other =
                document(json!({"environments": [{"name": "one", "extends": "realm"}]}));
            some.merge_from(&mut other, &Path::new("some/path")).unwrap();
            assert_eq!(
                to_value(some).unwrap(),
                json!({"environments": [{"name": "one", "extends": "realm"}]})
            );
        }
        {
            let mut some =
                document(json!({"environments": [{"name": "one", "__stop_timeout_ms": 10}]}));
            let mut other =
                document(json!({"environments": [{"name": "one", "__stop_timeout_ms": 10}]}));
            some.merge_from(&mut other, &Path::new("some/path")).unwrap();
            assert_eq!(
                to_value(some).unwrap(),
                json!({"environments": [{"name": "one", "__stop_timeout_ms": 10}]})
            );
        }
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
    fn test_canonicalize() {
        let mut some = document(json!({
            "children": [
                // Will be sorted by name
                { "name": "b_child", "url": "http://foo/b" },
                { "name": "a_child", "url": "http://foo/a" },
            ],
            "environments": [
                // Will be sorted by name
                { "name": "b_env" },
                { "name": "a_env" },
            ],
            "collections": [
                // Will be sorted by name
                { "name": "b_coll", "durability": "transient" },
                { "name": "a_coll", "durability": "transient" },
            ],
            // Will have entries sorted by capability type, then
            // by capability name (using the first entry in Many cases).
            "capabilities": [
                // Will be merged with "bar"
                { "protocol": ["foo"] },
                { "protocol": "bar" },
                // Will not be merged, but will be sorted before "bar"
                { "protocol": "arg", "path": "/arg" },
                // Will have list of names sorted
                { "service": ["b", "a"] },
                // Will have list of names sorted
                { "event_stream": ["b", "a"] },
                { "runner": "myrunner" },
                // The following two will *not* be merged, because they have a `path`.
                { "runner": "mypathrunner1", "path": "/foo" },
                { "runner": "mypathrunner2", "path": "/foo" },
            ],
            // Same rules as for "capabilities".
            "offer": [
                // Will be sorted after "bar"
                { "protocol": "baz", "from": "#a_child", "to": "#c_child"  },
                // The following two entries will be merged
                { "protocol": ["foo"], "from": "#a_child", "to": "#b_child"  },
                { "protocol": "bar", "from": "#a_child", "to": "#b_child"  },
                // Will have list of names sorted
                { "service": ["b", "a"], "from": "#a_child", "to": "#b_child"  },
                // Will have list of names sorted
                {
                    "event_stream": ["b", "a"],
                    "from": "#a_child",
                    "to": "#b_child",
                    "scope": ["#b", "#c", "#a"]  // Also gets sorted
                },
                { "runner": [ "myrunner", "a" ], "from": "#a_child", "to": "#b_child"  },
                { "runner": [ "b" ], "from": "#a_child", "to": "#b_child"  },
                { "directory": [ "b" ], "from": "#a_child", "to": "#b_child"  },
            ],
            "expose": [
                { "protocol": ["foo"], "from": "#a_child" },
                { "protocol": "bar", "from": "#a_child" },  // Will appear before protocol: foo
                // Will have list of names sorted
                { "service": ["b", "a"], "from": "#a_child" },
                // Will have list of names sorted
                {
                    "event_stream": ["b", "a"],
                    "from": "#a_child",
                    "scope": ["#b", "#c", "#a"]  // Also gets sorted
                },
                { "runner": [ "myrunner", "a" ], "from": "#a_child" },
                { "runner": [ "b" ], "from": "#a_child" },
                { "directory": [ "b" ], "from": "#a_child" },
            ],
            "use": [
                // Will be sorted after "baz"
                { "protocol": ["zazzle"], "path": "/zazbaz" },
                // These will be merged
                { "protocol": ["foo"] },
                { "protocol": "bar" },
                // Will have list of names sorted
                { "service": ["b", "a"] },
                // Will have list of names sorted
                { "event_stream": ["b", "a"], "scope": ["#b", "#a"] },
            ],
        }));
        some.canonicalize();

        assert_json_eq!(
            some,
            document(json!({
                "children": [
                    { "name": "a_child", "url": "http://foo/a" },
                    { "name": "b_child", "url": "http://foo/b" },
                ],
                "collections": [
                    { "name": "a_coll", "durability": "transient" },
                    { "name": "b_coll", "durability": "transient" },
                ],
                "environments": [
                    { "name": "a_env" },
                    { "name": "b_env" },
                ],
                "capabilities": [
                    { "event_stream": ["a", "b"] },
                    { "protocol": "arg", "path": "/arg" },
                    { "protocol": ["bar", "foo"] },
                    { "runner": "mypathrunner1", "path": "/foo" },
                    { "runner": "mypathrunner2", "path": "/foo" },
                    { "runner": "myrunner" },
                    { "service": ["a", "b"] },
                ],
                "use": [
                    { "event_stream": ["a", "b"], "scope": ["#a", "#b"] },
                    { "protocol": ["bar", "foo"] },
                    { "protocol": "zazzle", "path": "/zazbaz" },
                    { "service": ["a", "b"] },
                ],
                "offer": [
                    { "directory": "b", "from": "#a_child", "to": "#b_child" },
                    {
                        "event_stream": ["a", "b"],
                        "from": "#a_child",
                        "to": "#b_child",
                        "scope": ["#a", "#b", "#c"],
                    },
                    { "protocol": ["bar", "foo"], "from": "#a_child", "to": "#b_child" },
                    { "protocol": "baz", "from": "#a_child", "to": "#c_child"  },
                    { "runner": [ "a", "b", "myrunner" ], "from": "#a_child", "to": "#b_child" },
                    { "service": ["a", "b"], "from": "#a_child", "to": "#b_child" },
                ],
                "expose": [
                    { "directory": "b", "from": "#a_child" },
                    {
                        "event_stream": ["a", "b"],
                        "from": "#a_child",
                        "scope": ["#a", "#b", "#c"],
                    },
                    { "protocol": ["bar", "foo"], "from": "#a_child" },
                    { "runner": [ "a", "b", "myrunner" ], "from": "#a_child" },
                    { "service": ["a", "b"], "from": "#a_child" },
                ],
            }))
        )
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
        // https://fxbug.dev/42160240: merging with a document that doesn't have a runner doesn't override the
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

    #[test]
    fn test_merge_with_empty_names() {
        // This document is an error because there is no capability name.
        let mut my = document(json!({ "capabilities": [{ "path": "/a"}]}));

        let mut other = document(json!({ "capabilities": [
            { "directory": "a", "path": "/a" },
            { "directory": "b", "path": "/b" },
        ]}));
        my.merge_from(&mut other, &path::Path::new("some/path")).unwrap_err();
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
    #[test_case(&OfferFromRef::Named(Name::new("child".to_string()).unwrap()); "offer from named")]
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
