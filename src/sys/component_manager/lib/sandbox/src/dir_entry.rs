// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::connector::Connectable;
use crate::Connector;
use core::fmt;
use fidl::handle::{Channel, Status};
use fidl_fuchsia_io as fio;
use std::sync::Arc;
use vfs::directory::entry::{DirectoryEntry, OpenRequest};
use vfs::execution_scope::ExecutionScope;
use vfs::ToObjectRequest;

/// [DirEntry] is a [Capability] that's a thin wrapper over [vfs::directory::entry::DirectoryEntry]
/// When externalized to FIDL, a [DirEntry] becomes an opaque `eventpair` token. This means that
/// external users can delegate [DirEntry]s and put them in [Dict]s, but they cannot create their
/// own.
///
/// The [Capability::try_into_directory_entry] implementation simply extracts the inner
/// [vfs::directory::entry::DirectoryEntry] object.
///
/// [DirEntry] is a stopgap for representing CF capabilities that don't have a natural bedrock
/// representation yet. https://fxbug.dev/340891837 tracks its planned deletion.
///
#[derive(Clone)]
pub struct DirEntry {
    pub(crate) entry: Arc<dyn DirectoryEntry>,
}

impl Connectable for DirEntry {
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

impl DirEntry {
    /// Creates a [DirEntry] capability from a [vfs::directory::entry::DirectoryEntry].
    pub fn new(entry: Arc<dyn DirectoryEntry>) -> Self {
        Self { entry }
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
        server_end: Channel,
    ) {
        flags.to_object_request(server_end).handle(|object_request| {
            let path = path.validate()?;
            self.entry.clone().open_entry(OpenRequest::new(scope, flags, path, object_request))
        });
    }

    /// Forwards the open request.
    pub fn open_entry(&self, open_request: OpenRequest<'_>) -> Result<(), Status> {
        self.entry.clone().open_entry(open_request)
    }
}

impl fmt::Debug for DirEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirEntry").field("entry_type", &self.entry.entry_info().type_()).finish()
    }
}

impl From<Connector> for DirEntry {
    fn from(connector: Connector) -> Self {
        Self::new(vfs::service::endpoint(move |_scope, server_end| {
            let _ = connector.send_channel(server_end.into_zx_channel().into());
        }))
    }
}

pub trait ValidatePath {
    fn validate(self) -> Result<vfs::path::Path, Status>;
}

impl ValidatePath for vfs::path::Path {
    fn validate(self) -> Result<vfs::path::Path, Status> {
        Ok(self)
    }
}

impl ValidatePath for String {
    fn validate(self) -> Result<vfs::path::Path, Status> {
        vfs::path::Path::validate_and_split(self)
    }
}

impl ValidatePath for &str {
    fn validate(self) -> Result<vfs::path::Path, Status> {
        vfs::path::Path::validate_and_split(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fidl::RemotableCapability;
    use crate::{Capability, Dict, Receiver};
    use assert_matches::assert_matches;
    use fidl::endpoints::ClientEnd;
    use fidl::AsHandleRef;
    use futures::StreamExt;
    use {fidl_fuchsia_io as fio, fuchsia_async as fasync};

    #[fuchsia::test]
    async fn test_connector_into_open() {
        let (receiver, sender) = Receiver::new();
        let dir_entry = DirEntry::new(sender.try_into_directory_entry().unwrap());
        let (client_end, server_end) = Channel::create();
        let scope = ExecutionScope::new();
        dir_entry.open(scope, fio::OpenFlags::empty(), ".".to_owned(), server_end);
        let msg = receiver.receive().await.unwrap();
        assert_eq!(
            client_end.basic_info().unwrap().related_koid,
            msg.channel.basic_info().unwrap().koid
        );
    }

    #[test]
    fn test_connector_into_open_extra_path() {
        let mut ex = fasync::TestExecutor::new();

        let (receiver, sender) = Receiver::new();
        let dir_entry = DirEntry::new(sender.try_into_directory_entry().unwrap());
        let (client_end, server_end) = Channel::create();
        let scope = ExecutionScope::new();
        dir_entry.open(scope, fio::OpenFlags::empty(), "foo".to_owned(), server_end);

        let mut fut = std::pin::pin!(receiver.receive());
        assert!(ex.run_until_stalled(&mut fut).is_pending());

        let client_end: ClientEnd<fio::NodeMarker> = client_end.into();
        let node: fio::NodeProxy = client_end.into_proxy().unwrap();
        let result = ex.run_singlethreaded(node.take_event_stream().next()).unwrap();
        assert_matches!(
            result,
            Err(fidl::Error::ClientChannelClosed { status, .. })
            if status == Status::NOT_DIR
        );
    }

    #[fuchsia::test]
    async fn test_connector_into_open_via_dict() {
        let dict = Dict::new();
        let (receiver, sender) = Receiver::new();
        dict.insert("echo".parse().unwrap(), Capability::Connector(sender))
            .expect("dict entry already exists");

        let dir_entry = DirEntry::new(dict.try_into_directory_entry().unwrap());
        let (client_end, server_end) = Channel::create();
        let scope = ExecutionScope::new();
        dir_entry.open(scope, fio::OpenFlags::empty(), "echo".to_owned(), server_end);

        let msg = receiver.receive().await.unwrap();
        assert_eq!(
            client_end.basic_info().unwrap().related_koid,
            msg.channel.basic_info().unwrap().koid
        );
    }

    #[test]
    fn test_connector_into_open_via_dict_extra_path() {
        let mut ex = fasync::TestExecutor::new();

        let dict = Dict::new();
        let (receiver, sender) = Receiver::new();
        dict.insert("echo".parse().unwrap(), Capability::Connector(sender))
            .expect("dict entry already exists");

        let dir_entry = DirEntry::new(dict.try_into_directory_entry().unwrap());
        let (client_end, server_end) = Channel::create();
        let scope = ExecutionScope::new();
        dir_entry.open(scope, fio::OpenFlags::empty(), "echo/foo".to_owned(), server_end);

        let mut fut = std::pin::pin!(receiver.receive());
        assert!(ex.run_until_stalled(&mut fut).is_pending());

        let client_end: ClientEnd<fio::NodeMarker> = client_end.into();
        let node: fio::NodeProxy = client_end.into_proxy().unwrap();
        let result = ex.run_singlethreaded(node.take_event_stream().next()).unwrap();
        assert_matches!(
            result,
            Err(fidl::Error::ClientChannelClosed { status, .. })
            if status == Status::NOT_DIR
        );
    }
}
