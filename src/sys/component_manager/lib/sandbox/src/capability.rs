// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::Router;
use from_enum::FromEnum;
use router_error::Explain;
use std::fmt::Debug;
use thiserror::Error;
use zx_status;

#[derive(Error, Debug, Clone)]
pub enum ConversionError {
    #[error("invalid `fuchsia.io` node name: name `{0}` is too long")]
    ParseNameErrorTooLong(String),

    #[error("invalid `fuchsia.io` node name: name cannot be empty")]
    ParseNameErrorEmpty,

    #[error("invalid `fuchsia.io` node name: name cannot be `.`")]
    ParseNameErrorDot,

    #[error("invalid `fuchsia.io` node name: name cannot be `..`")]
    ParseNameErrorDotDot,

    #[error("invalid `fuchsia.io` node name: name cannot contain `/`")]
    ParseNameErrorSlash,

    #[error("invalid `fuchsia.io` node name: name cannot contain embedded NUL")]
    ParseNameErrorEmbeddedNul,

    #[error("conversion to type is not supported")]
    NotSupported,

    #[error("value at `{key}` could not be converted: {err}")]
    Nested {
        key: String,
        #[source]
        err: Box<ConversionError>,
    },
}

#[cfg(target_os = "fuchsia")]
impl From<vfs::name::ParseNameError> for ConversionError {
    fn from(parse_name_error: vfs::name::ParseNameError) -> Self {
        match parse_name_error {
            vfs::name::ParseNameError::TooLong(s) => ConversionError::ParseNameErrorTooLong(s),
            vfs::name::ParseNameError::Empty => ConversionError::ParseNameErrorEmpty,
            vfs::name::ParseNameError::Dot => ConversionError::ParseNameErrorDot,
            vfs::name::ParseNameError::DotDot => ConversionError::ParseNameErrorDotDot,
            vfs::name::ParseNameError::Slash => ConversionError::ParseNameErrorSlash,
            vfs::name::ParseNameError::EmbeddedNul => ConversionError::ParseNameErrorEmbeddedNul,
        }
    }
}

/// Errors arising from conversion between Rust and FIDL types.
#[derive(Error, Debug)]
pub enum RemoteError {
    #[error("unknown FIDL variant")]
    UnknownVariant,

    #[error("unregistered capability; only capabilities created by sandbox are allowed")]
    Unregistered,

    #[error("registered capability had the wrong type")]
    BadCapability,
}

impl Explain for RemoteError {
    fn as_zx_status(&self) -> zx_status::Status {
        match self {
            RemoteError::UnknownVariant => zx_status::Status::NOT_SUPPORTED,
            RemoteError::Unregistered => zx_status::Status::INVALID_ARGS,
            RemoteError::BadCapability => zx_status::Status::INVALID_ARGS,
        }
    }
}

#[derive(FromEnum, Debug)]
pub enum Capability {
    Unit(crate::Unit),
    Connector(crate::Connector),
    DirConnector(crate::DirConnector),
    Dictionary(crate::Dict),
    Data(crate::Data),
    Directory(crate::Directory),
    Handle(crate::Handle),
    ConnectorRouter(crate::Router<crate::Connector>),
    DictionaryRouter(crate::Router<crate::Dict>),
    DirEntryRouter(crate::Router<crate::DirEntry>),
    DirConnectorRouter(crate::Router<crate::DirConnector>),
    DataRouter(crate::Router<crate::Data>),
    Instance(crate::WeakInstanceToken),
    DirEntry(crate::DirEntry),
}

impl Capability {
    pub fn to_dictionary(self) -> Option<crate::Dict> {
        match self {
            Self::Dictionary(d) => Some(d),
            _ => None,
        }
    }

    pub fn try_clone(&self) -> Result<Self, ()> {
        let out = match self {
            Self::Connector(s) => Self::Connector(s.clone()),
            Self::DirConnector(s) => Self::DirConnector(s.clone()),
            Self::ConnectorRouter(s) => Self::ConnectorRouter(s.clone()),
            Self::DictionaryRouter(s) => Self::DictionaryRouter(s.clone()),
            Self::DirEntryRouter(s) => Self::DirEntryRouter(s.clone()),
            Self::DirConnectorRouter(s) => Self::DirConnectorRouter(s.clone()),
            Self::DataRouter(s) => Self::DataRouter(s.clone()),
            Self::Dictionary(s) => Self::Dictionary(s.clone()),
            Self::Data(s) => Self::Data(s.clone()),
            Self::Unit(s) => Self::Unit(s.clone()),
            Self::Directory(s) => Self::Directory(s.clone()),
            Self::Handle(s) => Self::Handle(s.try_clone()?),
            Self::Instance(s) => Self::Instance(s.clone()),
            Self::DirEntry(s) => Self::DirEntry(s.clone()),
        };
        Ok(out)
    }

    pub fn debug_typename(&self) -> &'static str {
        match self {
            Self::Connector(_) => crate::Connector::debug_typename(),
            Self::DirConnector(_) => crate::DirConnector::debug_typename(),
            Self::ConnectorRouter(_) => crate::Router::<crate::Connector>::debug_typename(),
            Self::DictionaryRouter(_) => crate::Router::<crate::Dict>::debug_typename(),
            Self::DirEntryRouter(_) => crate::Router::<crate::DirEntry>::debug_typename(),
            Self::DirConnectorRouter(_) => crate::Router::<crate::DirConnector>::debug_typename(),
            Self::DataRouter(_) => crate::Router::<crate::Data>::debug_typename(),
            Self::Dictionary(_) => crate::Dict::debug_typename(),
            Self::Data(_) => crate::Data::debug_typename(),
            Self::Unit(_) => crate::Unit::debug_typename(),
            Self::Directory(_) => crate::Directory::debug_typename(),
            Self::Handle(_) => crate::Handle::debug_typename(),
            Self::Instance(_) => "Instance",
            Self::DirEntry(_) => crate::DirEntry::debug_typename(),
        }
    }
}

impl TryFrom<Capability> for crate::Connector {
    type Error = ();

    fn try_from(c: Capability) -> Result<Self, Self::Error> {
        match c {
            Capability::Connector(c) => Ok(c),
            _ => Err(()),
        }
    }
}

impl TryFrom<Capability> for crate::DirConnector {
    type Error = ();

    fn try_from(c: Capability) -> Result<Self, Self::Error> {
        match c {
            Capability::DirConnector(c) => Ok(c),
            _ => Err(()),
        }
    }
}

impl TryFrom<Capability> for crate::Dict {
    type Error = ();

    fn try_from(c: Capability) -> Result<Self, Self::Error> {
        match c {
            Capability::Dictionary(c) => Ok(c),
            _ => Err(()),
        }
    }
}

impl TryFrom<Capability> for crate::Directory {
    type Error = ();

    fn try_from(c: Capability) -> Result<Self, Self::Error> {
        match c {
            Capability::Directory(c) => Ok(c),
            _ => Err(()),
        }
    }
}

impl TryFrom<Capability> for crate::DirEntry {
    type Error = ();

    fn try_from(c: Capability) -> Result<Self, Self::Error> {
        match c {
            Capability::DirEntry(c) => Ok(c),
            _ => Err(()),
        }
    }
}

impl TryFrom<Capability> for crate::Data {
    type Error = ();

    fn try_from(c: Capability) -> Result<Self, Self::Error> {
        match c {
            Capability::Data(c) => Ok(c),
            _ => Err(()),
        }
    }
}

impl TryFrom<Capability> for crate::Handle {
    type Error = ();

    fn try_from(c: Capability) -> Result<Self, Self::Error> {
        match c {
            Capability::Handle(r) => Ok(r),
            _ => Err(()),
        }
    }
}

impl TryFrom<Capability> for crate::Unit {
    type Error = ();

    fn try_from(c: Capability) -> Result<Self, Self::Error> {
        match c {
            Capability::Unit(r) => Ok(r),
            _ => Err(()),
        }
    }
}

impl TryFrom<Capability> for Router<crate::Dict> {
    type Error = ();

    fn try_from(c: Capability) -> Result<Self, Self::Error> {
        match c {
            Capability::DictionaryRouter(c) => Ok(c),
            _ => Err(()),
        }
    }
}

impl TryFrom<Capability> for Router<crate::DirConnector> {
    type Error = ();

    fn try_from(c: Capability) -> Result<Self, Self::Error> {
        match c {
            Capability::DirConnectorRouter(c) => Ok(c),
            _ => Err(()),
        }
    }
}

impl TryFrom<Capability> for Router<crate::DirEntry> {
    type Error = ();

    fn try_from(c: Capability) -> Result<Self, Self::Error> {
        match c {
            Capability::DirEntryRouter(c) => Ok(c),
            _ => Err(()),
        }
    }
}

impl TryFrom<Capability> for Router<crate::Connector> {
    type Error = ();

    fn try_from(c: Capability) -> Result<Self, Self::Error> {
        match c {
            Capability::ConnectorRouter(c) => Ok(c),
            _ => Err(()),
        }
    }
}

impl TryFrom<Capability> for Router<crate::Data> {
    type Error = ();

    fn try_from(c: Capability) -> Result<Self, Self::Error> {
        match c {
            Capability::DataRouter(c) => Ok(c),
            _ => Err(()),
        }
    }
}

/// Parent trait implemented by all capability types. Useful for defining interfaces that
/// generic over a capability type.
pub trait CapabilityBound: Into<Capability> + TryFrom<Capability> + Send + Sync + 'static {
    fn debug_typename() -> &'static str;
}
