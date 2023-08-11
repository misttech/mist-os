// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod builtin;
pub mod component_controller;
pub mod namespace;

pub use namespace::Entry as NamespaceEntry;
pub use namespace::{Namespace, NamespaceError};

use {
    async_trait::async_trait, fidl::endpoints::ServerEnd,
    fidl_fuchsia_component_runner as fcrunner, fidl_fuchsia_data as fdata, fidl_fuchsia_io as fio,
    fidl_fuchsia_mem as fmem, fidl_fuchsia_process as fprocess, fuchsia_zircon as zx,
    thiserror::Error,
};

/// Executes a component instance.
/// TODO: The runner should return a trait object to allow the component instance to be stopped,
/// binding to services, and observing abnormal termination.  In other words, a wrapper that
/// encapsulates fcrunner::ComponentController FIDL interfacing concerns.
#[async_trait]
pub trait Runner: Sync + Send {
    async fn start(
        &self,
        start_info: StartInfo,
        server_end: ServerEnd<fcrunner::ComponentControllerMarker>,
    );
}

#[derive(Debug, Error)]
pub enum StartInfoError {
    #[error("resolved_url is not set")]
    MissingResolvedUrl,

    #[error("invalid namespace")]
    NamespaceError(#[source] NamespaceError),
}

pub struct StartInfo {
    /// The resolved URL of the component.
    ///
    /// This is the canonical URL obtained by the component resolver after
    /// following redirects and resolving relative paths.
    pub resolved_url: String,

    /// The component's program declaration.
    /// This information originates from `ComponentDecl.program`.
    pub program: fdata::Dictionary,

    /// The namespace to provide to the component instance.
    ///
    /// A namespace specifies the set of directories that a component instance
    /// receives at start-up. Through the namespace directories, a component
    /// may access capabilities available to it. The contents of the namespace
    /// are mainly determined by the component's `use` declarations but may
    /// also contain additional capabilities automatically provided by the
    /// framework.
    ///
    /// By convention, a component's namespace typically contains some or all
    /// of the following directories:
    ///
    /// - "/svc": A directory containing services that the component requested
    ///           to use via its "import" declarations.
    /// - "/pkg": A directory containing the component's package, including its
    ///           binaries, libraries, and other assets.
    ///
    /// The mount points specified in each entry must be unique and
    /// non-overlapping. For example, [{"/foo", ..}, {"/foo/bar", ..}] is
    /// invalid.
    pub namespace: Namespace,

    /// The directory this component serves.
    pub outgoing_dir: Option<ServerEnd<fio::DirectoryMarker>>,

    /// The directory served by the runner to present runtime information about
    /// the component. The runner must either serve it, or drop it to avoid
    /// blocking any consumers indefinitely.
    pub runtime_dir: Option<ServerEnd<fio::DirectoryMarker>>,

    /// The numbered handles that were passed to the component.
    ///
    /// If the component does not support numbered handles, the runner is expected
    /// to close the handles.
    pub numbered_handles: Vec<fprocess::HandleInfo>,

    /// Binary representation of the component's configuration.
    ///
    /// # Layout
    ///
    /// The first 2 bytes of the data should be interpreted as an unsigned 16-bit
    /// little-endian integer which denotes the number of bytes following it that
    /// contain the configuration checksum. After the checksum, all the remaining
    /// bytes are a persistent FIDL message of a top-level struct. The struct's
    /// fields match the configuration fields of the component's compiled manifest
    /// in the same order.
    pub encoded_config: Option<fmem::Data>,

    /// An eventpair that debuggers can use to defer the launch of the component.
    ///
    /// For example, ELF runners hold off from creating processes in the component
    /// until ZX_EVENTPAIR_PEER_CLOSED is signaled on this eventpair. They also
    /// ensure that runtime_dir is served before waiting on this eventpair.
    /// ELF debuggers can query the runtime_dir to decide whether to attach before
    /// they drop the other side of the eventpair, which is sent in the payload of
    /// the DebugStarted event in fuchsia.component.events.
    pub break_on_start: Option<zx::EventPair>,
}

impl TryFrom<fcrunner::ComponentStartInfo> for StartInfo {
    type Error = StartInfoError;

    fn try_from(start_info: fcrunner::ComponentStartInfo) -> Result<Self, Self::Error> {
        let resolved_url =
            start_info.resolved_url.ok_or_else(|| StartInfoError::MissingResolvedUrl)?;
        let namespace = start_info.ns.map_or_else(
            || Ok(Namespace::default()),
            |ns| Namespace::try_from(ns).map_err(StartInfoError::NamespaceError),
        )?;

        Ok(Self {
            resolved_url,
            program: start_info.program.unwrap_or_else(|| fdata::Dictionary::default()),
            namespace,
            outgoing_dir: start_info.outgoing_dir,
            runtime_dir: start_info.runtime_dir,
            numbered_handles: start_info.numbered_handles.unwrap_or_else(|| Vec::new()),
            encoded_config: start_info.encoded_config,
            break_on_start: start_info.break_on_start,
        })
    }
}

impl From<StartInfo> for fcrunner::ComponentStartInfo {
    fn from(start_info: StartInfo) -> Self {
        Self {
            resolved_url: Some(start_info.resolved_url),
            program: Some(start_info.program),
            ns: Some(start_info.namespace.into()),
            outgoing_dir: start_info.outgoing_dir,
            runtime_dir: start_info.runtime_dir,
            numbered_handles: Some(start_info.numbered_handles),
            encoded_config: start_info.encoded_config,
            break_on_start: start_info.break_on_start,
            ..Default::default()
        }
    }
}
