// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{registry, sender::Sendable, CapabilityTrait, ConversionError};
use core::fmt;
use fidl::endpoints::{create_request_stream, ClientEnd};
use fidl_fuchsia_component_sandbox as fsandbox;
use fidl_fuchsia_io as fio;
use fuchsia_zircon::{self as zx, AsHandleRef};
use futures::TryStreamExt;
use std::sync::Arc;
use vfs::{
    directory::entry::{DirectoryEntry, OpenRequest, SubNode},
    execution_scope::ExecutionScope,
    remote::remote_dir,
    service::endpoint,
    ToObjectRequest,
};

/// An [Open] capability lets the holder obtain other capabilities by pipelining
/// a [zx::Channel], usually treated as the server endpoint of some FIDL protocol.
/// We call this operation opening the capability.
///
/// ## Open via remoting
///
/// The most straightforward way to open the capability is to convert it to a `fuchsia.io/Openable`
/// client end, via [Into<ClientEnd<fio::Openable>>]. FIDL open requests on this endpoint
/// translate to [OpenFn] calls.
///
/// Intuitively this is opening a new connection to the current object.
///
/// ## Open via `Dict` integration
///
/// When converting a `Dict` capability to [Open], all the dictionary entries will be
/// recursively converted to [Open] capabilities. An open capability within the `Dict`
/// functions similarly to a directory entry:
///
/// * Remoting the [Open] from the `Dict` gives access to a `fuchsia.io/Directory`.
/// * Within this directory, each member [Open] will show up as a directory entry, whose
///   type is `entry_type`.
/// * When a `fuchsia.io/Directory.Open` request hits the directory, the [OpenFn] of the
///   entry matching the first path segment of the open request will be invoked, passing
///   the remaining relative path if any, or "." if the path terminates at that entry.
///
/// Intuitively this is akin to mounting a remote VFS node in a directory.
#[derive(Clone)]
pub struct Open {
    entry: Arc<dyn DirectoryEntry>,
}

impl Sendable for Open {
    fn send(&self, message: crate::Message) -> Result<(), ()> {
        self.open(
            ExecutionScope::new(),
            fio::OpenFlags::empty(),
            vfs::path::Path::dot(),
            message.channel,
        );
        Ok(())
    }
}

impl Open {
    /// Creates an [Open] capability.
    ///
    /// Arguments:
    ///
    /// * `entry` - A VFS object that is responsible for handling the open.
    ///
    pub fn new(entry: Arc<dyn DirectoryEntry>) -> Self {
        Open { entry }
    }

    /// Opens the corresponding entry.
    ///
    /// If `path` fails validation, the `server_end` will be closed with a corresponding
    /// epitaph and optionally an event.
    pub fn open(
        &self,
        scope: ExecutionScope,
        flags: fio::OpenFlags,
        path: impl ValidatePath,
        server_end: zx::Channel,
    ) {
        flags.to_object_request(server_end).handle(|object_request| {
            let path = path.validate()?;
            self.entry.clone().open_entry(OpenRequest::new(scope, flags, path, object_request))
        });
    }

    /// Forwards the open request.
    pub fn open_entry(&self, open_request: OpenRequest<'_>) -> Result<(), zx::Status> {
        self.entry.clone().open_entry(open_request)
    }

    /// Returns an [`Open`] capability which will open paths relative to
    /// `relative_path` if non-empty, from the base [`Open`] object.
    ///
    /// The base capability is lazily exercised when the returned capability is exercised.
    ///
    /// Returns None if the underlying capability isn't directory-like.
    pub fn downscope_path(
        self,
        relative_path: vfs::path::Path,
        entry_type: fio::DirentType,
    ) -> Option<Open> {
        if self.entry.entry_info().type_() != fio::DirentType::Directory {
            None
        } else {
            Some(Open::new(Arc::new(SubNode::new(self.entry.clone(), relative_path, entry_type))))
        }
    }

    /// Turn the [Open] into a remote VFS node.
    ///
    /// Both `into_remote` and FIDL conversion will let us open the capability:
    ///
    /// * `into_remote` returns a type that implements `RemoteLike` and `DirectoryEntry` which
    ///   supports [RemoteLike::open].
    /// * FIDL conversion returns a client endpoint and calls
    ///   [DirectoryEntry::open] with the server endpoint given open requests.
    ///
    /// `into_remote` avoids a round trip through FIDL and channels, and is used as an
    /// internal performance optimization by `Dict` when building a directory tree.
    pub fn into_remote(self) -> Arc<dyn DirectoryEntry> {
        self.entry.clone()
    }

    /// Serves the `fuchsia.io.Openable` protocol for this Open and moves it into the registry.
    pub fn serve_and_register(self, mut stream: fio::OpenableRequestStream, koid: zx::Koid) {
        let open = self.clone();

        // Move this capability into the registry.
        registry::insert(self.into(), koid, async move {
            let scope = ExecutionScope::new();
            while let Ok(Some(request)) = stream.try_next().await {
                match request {
                    fio::OpenableRequest::Open { flags, mode: _, path, object, .. } => {
                        open.open(scope.clone(), flags, path, object.into_channel())
                    }
                }
            }
            scope.wait().await;
        });
    }
}

impl fmt::Debug for Open {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Open").field("entry_type", &self.entry.entry_info().type_()).finish()
    }
}

impl CapabilityTrait for Open {
    fn try_into_directory_entry(self) -> Result<Arc<dyn DirectoryEntry>, ConversionError> {
        Ok(self.entry)
    }
}

impl From<ClientEnd<fio::OpenableMarker>> for Open {
    fn from(value: ClientEnd<fio::OpenableMarker>) -> Self {
        // Open is one-way so a synchronous proxy is not going to block.
        let proxy = fio::OpenableSynchronousProxy::new(value.into_channel());
        Open::new(endpoint(move |_scope, server_end| {
            let _ = proxy.open(
                fio::OpenFlags::empty(),
                fio::ModeType::empty(),
                ".",
                server_end.into_zx_channel().into(),
            );
        }))
    }
}

impl From<ClientEnd<fio::DirectoryMarker>> for Open {
    fn from(value: ClientEnd<fio::DirectoryMarker>) -> Self {
        Open::new(remote_dir(value.into_proxy().unwrap()))
    }
}

impl From<Open> for ClientEnd<fio::OpenableMarker> {
    /// Serves the `fuchsia.io.Openable` protocol for this Open and moves it into the registry.
    fn from(open: Open) -> Self {
        let (client_end, openable_stream) = create_request_stream::<fio::OpenableMarker>().unwrap();
        open.serve_and_register(openable_stream, client_end.get_koid().unwrap());
        client_end
    }
}

impl From<Open> for fsandbox::Capability {
    fn from(open: Open) -> Self {
        Self::Sender(crate::Sender::new_sendable(open).into())
    }
}

pub trait ValidatePath {
    fn validate(self) -> Result<vfs::path::Path, zx::Status>;
}

impl ValidatePath for vfs::path::Path {
    fn validate(self) -> Result<vfs::path::Path, zx::Status> {
        Ok(self)
    }
}

impl ValidatePath for String {
    fn validate(self) -> Result<vfs::path::Path, zx::Status> {
        vfs::path::Path::validate_and_split(self)
    }
}

impl ValidatePath for &str {
    fn validate(self) -> Result<vfs::path::Path, zx::Status> {
        vfs::path::Path::validate_and_split(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Capability, Dict, Receiver};
    use anyhow::Result;
    use assert_matches::assert_matches;
    use fidl::endpoints::{create_endpoints, create_proxy};
    use fidl_fuchsia_io as fio;
    use fuchsia_async as fasync;
    use fuchsia_zircon as zx;
    use futures::StreamExt;
    use vfs::directory::{
        entry::serve_directory, entry_container::Directory as _, helper::DirectlyMutable,
        immutable::simple as pfs,
    };

    async fn get_entries(client_end: ClientEnd<fio::DirectoryMarker>) -> Result<Vec<String>> {
        let client = client_end.into_proxy()?;
        let entries = fuchsia_fs::directory::readdir(&client).await?;
        Ok(entries.into_iter().map(|entry| entry.name).collect())
    }

    #[fuchsia::test]
    async fn downscope_path() {
        // Build a directory tree with `/foo`.
        let dir = pfs::simple();
        dir.add_entry_impl("foo".to_owned().try_into().unwrap(), pfs::simple(), false).unwrap();

        // Make an [Open] that corresponds to `/`.
        let scope = ExecutionScope::new();
        let (dir_client_end, dir_server_end) = create_endpoints::<fio::DirectoryMarker>();
        dir.clone().open(
            scope.clone(),
            fio::OpenFlags::RIGHT_READABLE,
            vfs::path::Path::dot(),
            dir_server_end.into_channel().into(),
        );
        let open = Open::from(dir_client_end);

        // Verify that the connection has a directory named `foo`.
        let scope = ExecutionScope::new();
        let directory = serve_directory(
            open.clone().try_into_directory_entry().unwrap(),
            &scope,
            fio::OpenFlags::RIGHT_READABLE,
        )
        .unwrap();
        let entries = get_entries(directory).await.unwrap();
        assert_eq!(entries, vec!["foo".to_owned()]);

        // Downscope the path to `foo`.
        let open = open
            .clone()
            .downscope_path(
                vfs::path::Path::validate_and_split("foo".to_string()).unwrap(),
                fio::DirentType::Directory,
            )
            .unwrap();
        let directory = serve_directory(
            open.clone().try_into_directory_entry().unwrap(),
            &scope,
            fio::OpenFlags::RIGHT_READABLE,
        )
        .unwrap();

        // Verify that the connection does not have anymore children, since `foo` has no children.
        let entries = get_entries(directory).await.unwrap();
        assert_eq!(entries, vec![] as Vec<String>);
    }

    #[fuchsia::test]
    async fn invalid_path() {
        let (proxy, _server_end) = create_proxy::<fio::DirectoryMarker>().unwrap();
        let open = Open::new(remote_dir(proxy));

        let client_end: ClientEnd<fio::OpenableMarker> = open.into();
        let openable = client_end.into_proxy().unwrap();

        let (client_end, server_end) = fidl::endpoints::create_endpoints();
        openable.open(fio::OpenFlags::DESCRIBE, fio::ModeType::empty(), "..", server_end).unwrap();
        let proxy = client_end.into_proxy().unwrap();
        let mut event_stream = proxy.take_event_stream();

        let event = event_stream.try_next().await.unwrap().unwrap();
        let on_open = event.into_on_open_().unwrap();
        assert_eq!(on_open.0, zx::Status::INVALID_ARGS.into_raw());

        let event = event_stream.try_next().await;
        let error = event.unwrap_err();
        assert_matches!(
            error,
            fidl::Error::ClientChannelClosed { status, .. }
            if status == zx::Status::INVALID_ARGS
        );
    }

    #[fuchsia::test]
    async fn test_sender_into_open() {
        let (receiver, sender) = Receiver::new();
        let open = Open::new(sender.try_into_directory_entry().unwrap());
        let (client_end, server_end) = zx::Channel::create();
        let scope = ExecutionScope::new();
        open.open(scope, fio::OpenFlags::empty(), ".".to_owned(), server_end);
        let msg = receiver.receive().await.unwrap();
        assert_eq!(
            client_end.basic_info().unwrap().related_koid,
            msg.channel.basic_info().unwrap().koid
        );
    }

    #[test]
    fn test_sender_into_open_extra_path() {
        let mut ex = fasync::TestExecutor::new();

        let (receiver, sender) = Receiver::new();
        let open = Open::new(sender.try_into_directory_entry().unwrap());
        let (client_end, server_end) = zx::Channel::create();
        let scope = ExecutionScope::new();
        open.open(scope, fio::OpenFlags::empty(), "foo".to_owned(), server_end);

        let mut fut = std::pin::pin!(receiver.receive());
        assert!(ex.run_until_stalled(&mut fut).is_pending());

        let client_end: ClientEnd<fio::NodeMarker> = client_end.into();
        let node: fio::NodeProxy = client_end.into_proxy().unwrap();
        let result = ex.run_singlethreaded(node.take_event_stream().next()).unwrap();
        assert_matches!(
            result,
            Err(fidl::Error::ClientChannelClosed { status, .. })
            if status == zx::Status::NOT_DIR
        );
    }

    #[fuchsia::test]
    async fn test_sender_into_open_via_dict() {
        let mut dict = Dict::new();
        let (receiver, sender) = Receiver::new();
        dict.insert("echo".parse().unwrap(), Capability::Sender(sender))
            .expect("dict entry already exists");

        let open = Open::new(dict.try_into_directory_entry().unwrap());
        let (client_end, server_end) = zx::Channel::create();
        let scope = ExecutionScope::new();
        open.open(scope, fio::OpenFlags::empty(), "echo".to_owned(), server_end);

        let msg = receiver.receive().await.unwrap();
        assert_eq!(
            client_end.basic_info().unwrap().related_koid,
            msg.channel.basic_info().unwrap().koid
        );
    }

    #[test]
    fn test_sender_into_open_via_dict_extra_path() {
        let mut ex = fasync::TestExecutor::new();

        let mut dict = Dict::new();
        let (receiver, sender) = Receiver::new();
        dict.insert("echo".parse().unwrap(), Capability::Sender(sender))
            .expect("dict entry already exists");

        let open = Open::new(dict.try_into_directory_entry().unwrap());
        let (client_end, server_end) = zx::Channel::create();
        let scope = ExecutionScope::new();
        open.open(scope, fio::OpenFlags::empty(), "echo/foo".to_owned(), server_end);

        let mut fut = std::pin::pin!(receiver.receive());
        assert!(ex.run_until_stalled(&mut fut).is_pending());

        let client_end: ClientEnd<fio::NodeMarker> = client_end.into();
        let node: fio::NodeProxy = client_end.into_proxy().unwrap();
        let result = ex.run_singlethreaded(node.take_event_stream().next()).unwrap();
        assert_matches!(
            result,
            Err(fidl::Error::ClientChannelClosed { status, .. })
            if status == zx::Status::NOT_DIR
        );
    }
}
