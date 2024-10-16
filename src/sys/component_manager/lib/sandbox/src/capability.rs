// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::SpecificRouter;
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

    #[error("value at `{key}` could not be converted")]
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
    Dictionary(crate::Dict),
    Data(crate::Data),
    Directory(crate::Directory),
    Handle(crate::Handle),
    Router(crate::Router),
    ConnectorRouter(crate::SpecificRouter<crate::Connector>),
    DictionaryRouter(crate::SpecificRouter<crate::Dict>),
    DirEntryRouter(crate::SpecificRouter<crate::DirEntry>),
    DataRouter(crate::SpecificRouter<crate::Data>),
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
            Self::Router(s) => Self::Router(s.clone()),
            Self::ConnectorRouter(s) => Self::ConnectorRouter(s.clone()),
            Self::DictionaryRouter(s) => Self::DictionaryRouter(s.clone()),
            Self::DirEntryRouter(s) => Self::DirEntryRouter(s.clone()),
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
            Self::Connector(_) => "Connector",
            Self::Router(_) => "Router",
            Self::ConnectorRouter(_) => "ConnectorRouter",
            Self::DictionaryRouter(_) => "DictionaryRouter",
            Self::DirEntryRouter(_) => "DirEntryRouter",
            Self::DataRouter(_) => "DataRouter",
            Self::Dictionary(_) => "Dictionary",
            Self::Data(_) => "Data",
            Self::Unit(_) => "Unit",
            Self::Directory(_) => "Directory",
            Self::Handle(_) => "Handle",
            Self::Instance(_) => "Instance",
            Self::DirEntry(_) => "DirEntry",
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

impl TryFrom<Capability> for crate::Router {
    type Error = ();

    fn try_from(c: Capability) -> Result<Self, Self::Error> {
        match c {
            Capability::Router(r) => Ok(r),
            _ => Err(()),
        }
    }
}

impl TryFrom<Capability> for SpecificRouter<crate::Dict> {
    type Error = ();

    fn try_from(c: Capability) -> Result<Self, Self::Error> {
        match c {
            Capability::DictionaryRouter(c) => Ok(c),
            _ => Err(()),
        }
    }
}

impl TryFrom<Capability> for SpecificRouter<crate::DirEntry> {
    type Error = ();

    fn try_from(c: Capability) -> Result<Self, Self::Error> {
        match c {
            Capability::DirEntryRouter(c) => Ok(c),
            _ => Err(()),
        }
    }
}

impl TryFrom<Capability> for SpecificRouter<crate::Connector> {
    type Error = ();

    fn try_from(c: Capability) -> Result<Self, Self::Error> {
        match c {
            Capability::ConnectorRouter(c) => Ok(c),
            _ => Err(()),
        }
    }
}

impl TryFrom<Capability> for SpecificRouter<crate::Data> {
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
pub trait CapabilityBound: Into<Capability> + TryFrom<Capability> + Send + Sync + 'static {}
