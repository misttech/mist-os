// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        capability::CapabilitySource,
        model::{
            component::ComponentInstance,
            error::ModelError,
            routing::{self, RouteRequest, RouteSource},
            storage,
        },
    },
    fidl_fuchsia_io as fio, fuchsia_zircon as zx,
    std::{path::PathBuf, sync::Arc},
};

/// Contains the options to use when opening a capability.
///
/// See [`fidl_fuchsia_io::Directory::Open`] for how the `flags`, `relative_path`,
/// and `server_chan` parameters are used in the open call.
pub enum OpenOptions<'a> {
    Directory(OpenDirectoryOptions<'a>),
    EventStream(OpenEventStreamOptions<'a>),
    Protocol(OpenProtocolOptions<'a>),
    Resolver(OpenResolverOptions<'a>),
    Runner(OpenRunnerOptions<'a>),
    Service(OpenServiceOptions<'a>),
    Storage(OpenStorageOptions<'a>),
}

impl<'a> OpenOptions<'a> {
    /// Creates an `OpenOptions` for a capability that can be installed in a namespace,
    /// or an error if `route_request` specifies a capability that cannot be installed
    /// in a namespace.
    pub fn for_namespace_capability(
        route_request: &RouteRequest,
        flags: fio::OpenFlags,
        relative_path: String,
        server_chan: &'a mut zx::Channel,
    ) -> Result<Self, ModelError> {
        match route_request {
            RouteRequest::UseDirectory(_) | RouteRequest::ExposeDirectory(_) => {
                Ok(Self::Directory(OpenDirectoryOptions { flags, relative_path, server_chan }))
            }
            RouteRequest::UseProtocol(_) | RouteRequest::ExposeProtocol(_) => {
                Ok(Self::Protocol(OpenProtocolOptions { flags, relative_path, server_chan }))
            }
            RouteRequest::UseService(_) | RouteRequest::ExposeService(_) => {
                Ok(Self::Service(OpenServiceOptions { flags, relative_path, server_chan }))
            }
            RouteRequest::UseEventStream(_) => {
                Ok(Self::EventStream(OpenEventStreamOptions { flags, relative_path, server_chan }))
            }
            RouteRequest::UseStorage(_) => {
                Ok(Self::Storage(OpenStorageOptions { flags, relative_path, server_chan }))
            }
            _ => Err(ModelError::unsupported("capability cannot be installed in a namespace")),
        }
    }
}

pub struct OpenDirectoryOptions<'a> {
    pub flags: fio::OpenFlags,
    pub relative_path: String,
    pub server_chan: &'a mut zx::Channel,
}

pub struct OpenProtocolOptions<'a> {
    pub flags: fio::OpenFlags,
    pub relative_path: String,
    pub server_chan: &'a mut zx::Channel,
}

pub struct OpenResolverOptions<'a> {
    pub flags: fio::OpenFlags,
    pub server_chan: &'a mut zx::Channel,
}

pub struct OpenRunnerOptions<'a> {
    pub flags: fio::OpenFlags,
    pub server_chan: &'a mut zx::Channel,
}

pub struct OpenServiceOptions<'a> {
    pub flags: fio::OpenFlags,
    pub relative_path: String,
    pub server_chan: &'a mut zx::Channel,
}

pub struct OpenStorageOptions<'a> {
    pub flags: fio::OpenFlags,
    pub relative_path: String,
    pub server_chan: &'a mut zx::Channel,
}

pub struct OpenEventStreamOptions<'a> {
    pub flags: fio::OpenFlags,
    pub relative_path: String,
    pub server_chan: &'a mut zx::Channel,
}

/// A request to open a capability at its source.
pub enum OpenRequest<'a> {
    // Open a capability backed by a component's outgoing directory.
    OutgoingDirectory {
        flags: fio::OpenFlags,
        relative_path: PathBuf,
        source: CapabilitySource,
        target: &'a Arc<ComponentInstance>,
        server_chan: &'a mut zx::Channel,
    },
    // Open a storage capability.
    Storage {
        flags: fio::OpenFlags,
        relative_path: String,
        source: storage::StorageCapabilitySource,
        target: &'a Arc<ComponentInstance>,
        server_chan: &'a mut zx::Channel,
    },
}

impl<'a> OpenRequest<'a> {
    /// Creates a request to open a capability with source `route_source` for `target`.
    pub fn new_from_route_source(
        route_source: RouteSource,
        target: &'a Arc<ComponentInstance>,
        options: OpenOptions<'a>,
    ) -> Self {
        match route_source {
            RouteSource::Directory(source, directory_state) => {
                if let OpenOptions::Directory(open_dir_options) = options {
                    return Self::OutgoingDirectory {
                        flags: open_dir_options.flags,
                        relative_path: directory_state
                            .make_relative_path(open_dir_options.relative_path),
                        source,
                        target,
                        server_chan: open_dir_options.server_chan,
                    };
                }
            }
            RouteSource::Protocol(source) => {
                if let OpenOptions::Protocol(open_protocol_options) = options {
                    return Self::OutgoingDirectory {
                        flags: open_protocol_options.flags,
                        relative_path: PathBuf::from(open_protocol_options.relative_path),
                        source,
                        target,
                        server_chan: open_protocol_options.server_chan,
                    };
                }
            }
            RouteSource::Service(source) => {
                if let OpenOptions::Service(open_service_options) = options {
                    return Self::OutgoingDirectory {
                        flags: open_service_options.flags,
                        relative_path: PathBuf::from(open_service_options.relative_path),
                        source,
                        target,
                        server_chan: open_service_options.server_chan,
                    };
                }
            }
            RouteSource::Resolver(source) => {
                if let OpenOptions::Resolver(open_resolver_options) = options {
                    return Self::OutgoingDirectory {
                        flags: open_resolver_options.flags,
                        relative_path: PathBuf::new(),
                        source,
                        target,
                        server_chan: open_resolver_options.server_chan,
                    };
                }
            }
            RouteSource::Runner(source) => {
                if let OpenOptions::Runner(open_runner_options) = options {
                    return Self::OutgoingDirectory {
                        flags: open_runner_options.flags,
                        relative_path: PathBuf::new(),
                        source,
                        target,
                        server_chan: open_runner_options.server_chan,
                    };
                }
            }
            RouteSource::EventStream(source) => {
                if let OpenOptions::EventStream(open_event_stream_options) = options {
                    return Self::OutgoingDirectory {
                        flags: open_event_stream_options.flags,
                        relative_path: PathBuf::from(open_event_stream_options.relative_path),
                        source,
                        target,
                        server_chan: open_event_stream_options.server_chan,
                    };
                }
            }
            RouteSource::StorageBackingDirectory(source) => {
                if let OpenOptions::Storage(open_storage_options) = options {
                    return Self::Storage {
                        flags: open_storage_options.flags,
                        relative_path: open_storage_options.relative_path,
                        source,
                        target,
                        server_chan: open_storage_options.server_chan,
                    };
                }
            }
            RouteSource::Storage(_) | RouteSource::Event(_) => panic!("unsupported route source"),
        }
        panic!("route source type did not match option type")
    }

    /// Directly creates a request to open a capability at `source`'s outgoing directory with
    /// with `flags` and `relative_path`.
    pub fn new_outgoing_directory(
        flags: fio::OpenFlags,
        relative_path: PathBuf,
        source: CapabilitySource,
        target: &'a Arc<ComponentInstance>,
        server_chan: &'a mut zx::Channel,
    ) -> Self {
        Self::OutgoingDirectory { flags, relative_path, source, target, server_chan }
    }

    /// Opens the capability in `self`, triggering a `CapabilityRouted` event and binding
    /// to the source component instance if necessary.
    pub async fn open(self) -> Result<(), ModelError> {
        match self {
            Self::OutgoingDirectory { flags, relative_path, source, target, server_chan } => {
                routing::open_capability_at_source(
                    flags,
                    relative_path,
                    source,
                    target,
                    server_chan,
                )
                .await
            }
            Self::Storage { flags, relative_path, source, target, server_chan } => {
                routing::open_storage_capability(flags, relative_path, &source, target, server_chan)
                    .await
            }
        }
    }
}
