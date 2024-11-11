// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::CapabilityBound;
use std::fmt;

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
/// When built for host, [DirEntry] is an empty placeholder type, as vfs is not supported on host.
#[derive(Clone)]
pub struct DirEntry {
    #[cfg(target_os = "fuchsia")]
    pub(crate) entry: std::sync::Arc<dyn vfs::directory::entry::DirectoryEntry>,
}

impl CapabilityBound for DirEntry {
    fn debug_typename() -> &'static str {
        "DirEntry"
    }
}

#[cfg(target_os = "fuchsia")]
impl fmt::Debug for DirEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirEntry").field("entry_type", &self.entry.entry_info().type_()).finish()
    }
}

#[cfg(not(target_os = "fuchsia"))]
impl fmt::Debug for DirEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirEntry").finish()
    }
}

#[cfg(target_os = "fuchsia")]
#[cfg(test)]
mod tests {
    use crate::fidl::RemotableCapability;
    use crate::{Capability, Connector, Dict, DirEntry};
    use assert_matches::assert_matches;
    use fidl::endpoints::ClientEnd;
    use fidl::{AsHandleRef, Channel};
    use futures::StreamExt;
    use vfs::execution_scope::ExecutionScope;
    use zx::Status;
    use {fidl_fuchsia_io as fio, fuchsia_async as fasync};

    #[fuchsia::test]
    async fn test_connector_into_open() {
        let (receiver, sender) = Connector::new();
        let scope = ExecutionScope::new();
        let dir_entry = DirEntry::new(sender.try_into_directory_entry(scope.clone()).unwrap());
        let (client_end, server_end) = Channel::create();
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

        let (receiver, sender) = Connector::new();
        let scope = ExecutionScope::new();
        let dir_entry = DirEntry::new(sender.try_into_directory_entry(scope.clone()).unwrap());
        let (client_end, server_end) = Channel::create();
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
        let (receiver, sender) = Connector::new();
        dict.insert("echo".parse().unwrap(), Capability::Connector(sender))
            .expect("dict entry already exists");

        let scope = ExecutionScope::new();
        let dir_entry = DirEntry::new(dict.try_into_directory_entry(scope.clone()).unwrap());
        let (client_end, server_end) = Channel::create();
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
        let (receiver, sender) = Connector::new();
        dict.insert("echo".parse().unwrap(), Capability::Connector(sender))
            .expect("dict entry already exists");

        let scope = ExecutionScope::new();
        let dir_entry = DirEntry::new(dict.try_into_directory_entry(scope.clone()).unwrap());
        let (client_end, server_end) = Channel::create();
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
