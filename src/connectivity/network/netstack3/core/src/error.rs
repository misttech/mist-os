// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Custom error types for the netstack.

use net_types::ip::{GenericOverIp, Ip};
use thiserror::Error;

/// Error when something is not supported.
#[derive(Debug, PartialEq, Eq, Error, GenericOverIp)]
#[generic_over_ip()]
#[error("Not supported")]
pub struct NotSupportedError;

/// Error when something exists unexpectedly.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("Already exists")]
pub struct ExistsError;

impl From<ExistsError> for SocketError {
    fn from(_: ExistsError) -> SocketError {
        SocketError::Local(LocalAddressError::AddressInUse)
    }
}

/// Error when something unexpectedly doesn't exist, such as trying to
/// remove an element when the element is not present.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("Not found")]
pub struct NotFoundError;

/// Error type for errors common to local addresses.
#[derive(Error, Debug, PartialEq, GenericOverIp)]
#[generic_over_ip()]
pub enum LocalAddressError {
    /// Cannot bind to address.
    #[error("can't bind to address")]
    CannotBindToAddress,

    /// Failed to allocate local port.
    #[error("failed to allocate local port")]
    FailedToAllocateLocalPort,

    /// Specified local address does not match any expected address.
    #[error("specified local address does not match any expected address")]
    AddressMismatch,

    /// The requested address/socket pair is in use.
    #[error("Address in use")]
    AddressInUse,

    /// The address cannot be used because of its zone.
    ///
    /// TODO(https://fxbug.dev/42054471): Make this an IP socket error once UDP
    /// sockets contain IP sockets.
    #[error("{}", _0)]
    Zone(#[from] ZonedAddressError),

    /// The requested address is mapped (i.e. an IPv4-mapped-IPv6 address), but
    /// the socket is not dual-stack enabled.
    #[error("Address is mapped")]
    AddressUnexpectedlyMapped,
}

/// Indicates a problem related to an address with a zone.
#[derive(Copy, Clone, Debug, Error, Eq, PartialEq, GenericOverIp)]
#[generic_over_ip()]
pub enum ZonedAddressError {
    /// The address scope requires a zone but didn't have one.
    #[error("the address requires a zone but didn't have one")]
    RequiredZoneNotProvided,
    /// The address has a zone that doesn't match an existing device constraint.
    #[error("the socket's device does not match the zone")]
    DeviceZoneMismatch,
}

/// An error encountered when attempting to create a UDP, TCP, or ICMP connection.
#[derive(Error, Debug, PartialEq)]
pub enum RemoteAddressError {
    /// No route to host.
    #[error("no route to host")]
    NoRoute,
}

/// Error type for connection errors.
#[derive(Error, Debug, PartialEq)]
pub enum SocketError {
    #[error("{}", _0)]
    /// Errors related to the local address.
    Local(#[from] LocalAddressError),

    #[error("{}", _0)]
    /// Errors related to the remote address.
    Remote(RemoteAddressError),
}

/// Error when link address resolution failed for a neighbor.
#[derive(Error, Debug, PartialEq)]
#[error("Address resolution failed")]
pub struct AddressResolutionFailed;
