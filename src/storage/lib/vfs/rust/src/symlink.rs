// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Server support for symbolic links.

use crate::common::{
    decode_extended_attribute_value, encode_extended_attribute_value, extended_attributes_sender,
    inherit_rights_for_clone, send_on_open_with_error,
};
use crate::execution_scope::ExecutionScope;
use crate::name::parse_name;
use crate::node::Node;
use crate::object_request::Representation;
use crate::{ObjectRequest, ObjectRequestRef, ProtocolsExt, ToObjectRequest};
use fidl::endpoints::{ControlHandle as _, Responder, ServerEnd};
use fidl_fuchsia_io as fio;
use futures::StreamExt;
use std::future::{ready, Future};
use std::sync::Arc;
use zx_status::Status;

pub trait Symlink: Node {
    fn read_target(&self) -> impl Future<Output = Result<Vec<u8>, Status>> + Send;

    // Extended attributes for symlinks.
    fn list_extended_attributes(
        &self,
    ) -> impl Future<Output = Result<Vec<Vec<u8>>, Status>> + Send {
        ready(Err(Status::NOT_SUPPORTED))
    }
    fn get_extended_attribute(
        &self,
        _name: Vec<u8>,
    ) -> impl Future<Output = Result<Vec<u8>, Status>> + Send {
        ready(Err(Status::NOT_SUPPORTED))
    }
    fn set_extended_attribute(
        &self,
        _name: Vec<u8>,
        _value: Vec<u8>,
        _mode: fio::SetExtendedAttributeMode,
    ) -> impl Future<Output = Result<(), Status>> + Send {
        ready(Err(Status::NOT_SUPPORTED))
    }
    fn remove_extended_attribute(
        &self,
        _name: Vec<u8>,
    ) -> impl Future<Output = Result<(), Status>> + Send {
        ready(Err(Status::NOT_SUPPORTED))
    }
}

pub struct Connection<T> {
    scope: ExecutionScope,
    symlink: Arc<T>,
}

pub struct SymlinkOptions;

impl<T: Symlink> Connection<T> {
    /// Returns a new connection object.
    pub fn new(scope: ExecutionScope, symlink: Arc<T>) -> Self {
        Self { scope, symlink }
    }

    /// Upon success, returns a future that will proceess requests for the connection.
    pub fn create(
        scope: ExecutionScope,
        symlink: Arc<T>,
        protocols: &impl ProtocolsExt,
        object_request: ObjectRequestRef<'_>,
    ) -> Result<impl Future<Output = ()>, Status> {
        let _options = protocols.to_symlink_options()?;
        Ok(Connection::new(scope, symlink).run(object_request.take()))
    }

    /// Process requests until either `shutdown` is notfied, the connection is closed, or an error
    /// with the connection is encountered.
    pub async fn run(mut self, object_request: ObjectRequest) {
        if let Ok(mut requests) = object_request.into_request_stream(&self).await {
            while let Some(Ok(request)) = requests.next().await {
                let _guard = self.scope.active_guard();
                if self.handle_request(request).await.unwrap_or(true) {
                    break;
                }
            }
        }
    }

    // Returns true if the connection should terminate.
    async fn handle_request(&mut self, req: fio::SymlinkRequest) -> Result<bool, fidl::Error> {
        match req {
            fio::SymlinkRequest::Clone { flags, object, control_handle: _ } => {
                self.handle_clone(flags, object);
            }
            fio::SymlinkRequest::Clone2 { request, control_handle: _ } => {
                self.handle_clone2(ServerEnd::new(request.into_channel()));
            }
            fio::SymlinkRequest::Close { responder } => {
                responder.send(Ok(()))?;
                return Ok(true);
            }
            fio::SymlinkRequest::LinkInto { dst_parent_token, dst, responder } => {
                responder.send(
                    self.handle_link_into(dst_parent_token, dst).await.map_err(|s| s.into_raw()),
                )?;
            }
            fio::SymlinkRequest::GetConnectionInfo { responder } => {
                // TODO(https://fxbug.dev/293947862): Restrict GET_ATTRIBUTES.
                let rights = fio::Operations::GET_ATTRIBUTES;
                responder
                    .send(fio::ConnectionInfo { rights: Some(rights), ..Default::default() })?;
            }
            fio::SymlinkRequest::Sync { responder } => {
                responder.send(Ok(()))?;
            }
            fio::SymlinkRequest::GetAttr { responder } => {
                // TODO(https://fxbug.dev/293947862): Restrict GET_ATTRIBUTES.
                let (status, attrs) = crate::common::io2_to_io1_attrs(
                    self.symlink.as_ref(),
                    fio::Rights::GET_ATTRIBUTES,
                )
                .await;
                responder.send(status.into_raw(), &attrs)?;
            }
            fio::SymlinkRequest::SetAttr { responder, .. } => {
                responder.send(Status::ACCESS_DENIED.into_raw())?;
            }
            fio::SymlinkRequest::GetAttributes { query, responder } => {
                // TODO(https://fxbug.dev/293947862): Restrict GET_ATTRIBUTES.
                let attrs = self.symlink.get_attributes(query).await;
                responder.send(
                    attrs
                        .as_ref()
                        .map(|attrs| (&attrs.mutable_attributes, &attrs.immutable_attributes))
                        .map_err(|status| status.into_raw()),
                )?;
            }
            fio::SymlinkRequest::UpdateAttributes { payload: _, responder } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::SymlinkRequest::ListExtendedAttributes { iterator, control_handle: _ } => {
                self.handle_list_extended_attribute(iterator).await;
            }
            fio::SymlinkRequest::GetExtendedAttribute { responder, name } => {
                let res = self.handle_get_extended_attribute(name).await.map_err(|s| s.into_raw());
                responder.send(res)?;
            }
            fio::SymlinkRequest::SetExtendedAttribute { responder, name, value, mode } => {
                let res = self
                    .handle_set_extended_attribute(name, value, mode)
                    .await
                    .map_err(|s| s.into_raw());
                responder.send(res)?;
            }
            fio::SymlinkRequest::RemoveExtendedAttribute { responder, name } => {
                let res =
                    self.handle_remove_extended_attribute(name).await.map_err(|s| s.into_raw());
                responder.send(res)?;
            }
            fio::SymlinkRequest::Describe { responder } => match self.symlink.read_target().await {
                Ok(target) => responder
                    .send(&fio::SymlinkInfo { target: Some(target), ..Default::default() })?,
                Err(status) => {
                    responder.control_handle().shutdown_with_epitaph(status);
                    return Ok(true);
                }
            },
            fio::SymlinkRequest::GetFlags { responder } => {
                responder.send(Status::NOT_SUPPORTED.into_raw(), fio::OpenFlags::empty())?;
            }
            fio::SymlinkRequest::SetFlags { responder, .. } => {
                responder.send(Status::ACCESS_DENIED.into_raw())?;
            }
            fio::SymlinkRequest::Query { responder } => {
                responder.send(fio::SYMLINK_PROTOCOL_NAME.as_bytes())?;
            }
            fio::SymlinkRequest::QueryFilesystem { responder } => {
                match self.symlink.query_filesystem() {
                    Err(status) => responder.send(status.into_raw(), None)?,
                    Ok(info) => responder.send(0, Some(&info))?,
                }
            }
            #[cfg(fuchsia_api_level_at_least = "HEAD")]
            fio::SymlinkRequest::GetFlags2 { responder } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            #[cfg(fuchsia_api_level_at_least = "HEAD")]
            fio::SymlinkRequest::SetFlags2 { flags: _, responder } => {
                responder.send(Err(Status::NOT_SUPPORTED.into_raw()))?;
            }
            fio::SymlinkRequest::_UnknownMethod { ordinal, .. } => {
                tracing::warn!(ordinal, "Received unknown method")
            }
        }
        Ok(false)
    }

    fn handle_clone(&mut self, flags: fio::OpenFlags, server_end: ServerEnd<fio::NodeMarker>) {
        let flags = match inherit_rights_for_clone(fio::OpenFlags::RIGHT_READABLE, flags) {
            Ok(updated) => updated,
            Err(status) => {
                send_on_open_with_error(
                    flags.contains(fio::OpenFlags::DESCRIBE),
                    server_end,
                    status,
                );
                return;
            }
        };
        flags.to_object_request(server_end).handle(|object_request| {
            self.scope.spawn(Self::create(
                self.scope.clone(),
                self.symlink.clone(),
                &flags,
                object_request,
            )?);
            Ok(())
        });
    }

    fn handle_clone2(&mut self, server_end: ServerEnd<fio::SymlinkMarker>) {
        let flags = fio::Flags::PROTOCOL_SYMLINK | fio::Flags::PERM_GET_ATTRIBUTES;
        flags.to_object_request(server_end).handle(|object_request| {
            self.scope.spawn(Self::create(
                self.scope.clone(),
                self.symlink.clone(),
                &flags,
                object_request,
            )?);
            Ok(())
        });
    }

    async fn handle_link_into(
        &mut self,
        target_parent_token: fidl::Event,
        target_name: String,
    ) -> Result<(), Status> {
        let target_name = parse_name(target_name).map_err(|_| Status::INVALID_ARGS)?;

        let target_parent = self
            .scope
            .token_registry()
            .get_owner(target_parent_token.into())?
            .ok_or(Err(Status::NOT_FOUND))?;

        self.symlink.clone().link_into(target_parent, target_name).await
    }

    async fn handle_list_extended_attribute(
        &self,
        iterator: ServerEnd<fio::ExtendedAttributeIteratorMarker>,
    ) {
        let attributes = match self.symlink.list_extended_attributes().await {
            Ok(attributes) => attributes,
            Err(status) => {
                tracing::error!(?status, "list extended attributes failed");
                iterator
                    .close_with_epitaph(status)
                    .unwrap_or_else(|error| tracing::error!(?error, "failed to send epitaph"));
                return;
            }
        };
        self.scope.spawn(extended_attributes_sender(iterator, attributes));
    }

    async fn handle_get_extended_attribute(
        &self,
        name: Vec<u8>,
    ) -> Result<fio::ExtendedAttributeValue, Status> {
        let value = self.symlink.get_extended_attribute(name).await?;
        encode_extended_attribute_value(value)
    }

    async fn handle_set_extended_attribute(
        &self,
        name: Vec<u8>,
        value: fio::ExtendedAttributeValue,
        mode: fio::SetExtendedAttributeMode,
    ) -> Result<(), Status> {
        if name.iter().any(|c| *c == 0) {
            return Err(Status::INVALID_ARGS);
        }
        let val = decode_extended_attribute_value(value)?;
        self.symlink.set_extended_attribute(name, val, mode).await
    }

    async fn handle_remove_extended_attribute(&self, name: Vec<u8>) -> Result<(), Status> {
        self.symlink.remove_extended_attribute(name).await
    }
}

impl<T: Symlink> Representation for Connection<T> {
    type Protocol = fio::SymlinkMarker;

    async fn get_representation(
        &self,
        requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::Representation, Status> {
        Ok(fio::Representation::Symlink(fio::SymlinkInfo {
            attributes: if requested_attributes.is_empty() {
                None
            } else {
                Some(self.symlink.get_attributes(requested_attributes).await?)
            },
            target: Some(self.symlink.read_target().await?),
            ..Default::default()
        }))
    }

    async fn node_info(&self) -> Result<fio::NodeInfoDeprecated, Status> {
        Ok(fio::NodeInfoDeprecated::Symlink(fio::SymlinkObject {
            target: self.symlink.read_target().await?,
        }))
    }
}

/// Helper to open a service or node as required.
pub fn serve(
    link: Arc<impl Symlink>,
    scope: ExecutionScope,
    protocols: &impl ProtocolsExt,
    object_request: ObjectRequestRef<'_>,
) -> Result<(), Status> {
    if protocols.is_node() {
        let options = protocols.to_node_options(link.entry_info().type_())?;
        link.open_as_node(scope, options, object_request)
    } else {
        Connection::create(scope.clone(), link, protocols, object_request).map(|x| {
            scope.spawn(x);
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Connection, Symlink};
    use crate::common::rights_to_posix_mode_bits;
    use crate::directory::entry::{EntryInfo, GetEntryInfo};
    use crate::execution_scope::ExecutionScope;
    use crate::node::Node;
    use crate::{immutable_attributes, ToObjectRequest};
    use assert_matches::assert_matches;
    use fidl::endpoints::{create_proxy, ServerEnd};
    use fidl_fuchsia_io as fio;
    use futures::StreamExt;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use zx_status::Status;

    const TARGET: &[u8] = b"target";

    struct TestSymlink {
        xattrs: Mutex<HashMap<Vec<u8>, Vec<u8>>>,
    }

    impl TestSymlink {
        fn new() -> Self {
            TestSymlink { xattrs: Mutex::new(HashMap::new()) }
        }
    }

    impl Symlink for TestSymlink {
        async fn read_target(&self) -> Result<Vec<u8>, Status> {
            Ok(TARGET.to_vec())
        }
        async fn list_extended_attributes(&self) -> Result<Vec<Vec<u8>>, Status> {
            let map = self.xattrs.lock().unwrap();
            Ok(map.values().map(|x| x.clone()).collect())
        }
        async fn get_extended_attribute(&self, name: Vec<u8>) -> Result<Vec<u8>, Status> {
            let map = self.xattrs.lock().unwrap();
            map.get(&name).map(|x| x.clone()).ok_or(Status::NOT_FOUND)
        }
        async fn set_extended_attribute(
            &self,
            name: Vec<u8>,
            value: Vec<u8>,
            _mode: fio::SetExtendedAttributeMode,
        ) -> Result<(), Status> {
            let mut map = self.xattrs.lock().unwrap();
            // Don't bother replicating the mode behavior, we just care that this method is hooked
            // up at all.
            map.insert(name, value);
            Ok(())
        }
        async fn remove_extended_attribute(&self, name: Vec<u8>) -> Result<(), Status> {
            let mut map = self.xattrs.lock().unwrap();
            map.remove(&name);
            Ok(())
        }
    }

    impl Node for TestSymlink {
        async fn get_attributes(
            &self,
            requested_attributes: fio::NodeAttributesQuery,
        ) -> Result<fio::NodeAttributes2, Status> {
            Ok(immutable_attributes!(
                requested_attributes,
                Immutable {
                    content_size: TARGET.len() as u64,
                    storage_size: TARGET.len() as u64,
                    protocols: fio::NodeProtocolKinds::SYMLINK,
                    abilities: fio::Abilities::GET_ATTRIBUTES,
                }
            ))
        }
    }

    impl GetEntryInfo for TestSymlink {
        fn entry_info(&self) -> EntryInfo {
            EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Symlink)
        }
    }

    #[fuchsia::test]
    async fn test_read_target() {
        let scope = ExecutionScope::new();
        let (client_end, server_end) = create_proxy::<fio::SymlinkMarker>();

        scope.spawn(
            Connection::new(scope.clone(), Arc::new(TestSymlink::new()))
                .run(fio::OpenFlags::RIGHT_READABLE.to_object_request(server_end)),
        );

        assert_eq!(
            client_end.describe().await.expect("fidl failed").target.expect("missing target"),
            b"target"
        );
    }

    #[fuchsia::test]
    async fn test_validate_flags() {
        let scope = ExecutionScope::new();

        let check = |mut flags: fio::OpenFlags| {
            let (client_end, server_end) = create_proxy::<fio::SymlinkMarker>();
            flags |= fio::OpenFlags::DESCRIBE;
            flags.to_object_request(server_end).handle(|object_request| {
                object_request.spawn_connection(
                    scope.clone(),
                    Arc::new(TestSymlink::new()),
                    flags,
                    |scope, symlink, protocols, object_request| {
                        Connection::create(scope, symlink, &protocols, object_request)
                    },
                )
            });
            async move {
                Status::from_raw(
                    client_end
                        .take_event_stream()
                        .next()
                        .await
                        .expect("no event")
                        .expect("next failed")
                        .into_on_open_()
                        .expect("expected OnOpen")
                        .0,
                )
            }
        };

        for flags in [
            fio::OpenFlags::RIGHT_WRITABLE,
            fio::OpenFlags::RIGHT_EXECUTABLE,
            fio::OpenFlags::CREATE,
            fio::OpenFlags::CREATE_IF_ABSENT,
            fio::OpenFlags::TRUNCATE,
            fio::OpenFlags::APPEND,
            fio::OpenFlags::POSIX_WRITABLE,
            fio::OpenFlags::POSIX_EXECUTABLE,
            fio::OpenFlags::CLONE_SAME_RIGHTS,
            fio::OpenFlags::BLOCK_DEVICE,
        ] {
            assert_eq!(check(flags).await, Status::INVALID_ARGS, "{flags:?}");
        }

        assert_eq!(
            check(fio::OpenFlags::RIGHT_READABLE | fio::OpenFlags::NOT_DIRECTORY).await,
            Status::OK
        );
    }

    #[fuchsia::test]
    async fn test_get_attr() {
        let scope = ExecutionScope::new();
        let (client_end, server_end) = create_proxy::<fio::SymlinkMarker>();

        scope.spawn(
            Connection::new(scope.clone(), Arc::new(TestSymlink::new()))
                .run(fio::OpenFlags::RIGHT_READABLE.to_object_request(server_end)),
        );

        assert_matches!(
            client_end.get_attr().await.expect("fidl failed"),
            (
                0,
                fio::NodeAttributes {
                    mode,
                    id: fio::INO_UNKNOWN,
                    content_size: 6,
                    storage_size: 6,
                    link_count: 1,
                    creation_time: 0,
                    modification_time: 0,
                }
            ) if mode == fio::MODE_TYPE_SYMLINK
                | rights_to_posix_mode_bits(/*r*/ true, /*w*/ false, /*x*/ false)
        );
    }

    #[fuchsia::test]
    async fn test_clone2() {
        let scope = ExecutionScope::new();
        let (client_end, server_end) = create_proxy::<fio::SymlinkMarker>();
        scope.spawn(
            Connection::new(scope.clone(), Arc::new(TestSymlink::new()))
                .run(fio::Flags::PERM_GET_ATTRIBUTES.to_object_request(server_end)),
        );
        let orig_attrs = client_end
            .get_attributes(fio::NodeAttributesQuery::all())
            .await
            .expect("fidl failed")
            .unwrap();
        // Clone the original connection and query it's attributes, which should match the original.
        let (cloned_client, cloned_server) = create_proxy::<fio::SymlinkMarker>();
        client_end.clone2(ServerEnd::new(cloned_server.into_channel())).unwrap();
        let cloned_attrs = cloned_client
            .get_attributes(fio::NodeAttributesQuery::all())
            .await
            .expect("fidl failed")
            .unwrap();
        assert_eq!(orig_attrs, cloned_attrs);
    }

    #[fuchsia::test]
    async fn test_describe() {
        let scope = ExecutionScope::new();
        let (client_end, server_end) = create_proxy::<fio::SymlinkMarker>();

        scope.spawn(
            Connection::new(scope.clone(), Arc::new(TestSymlink::new()))
                .run(fio::OpenFlags::RIGHT_READABLE.to_object_request(server_end)),
        );

        assert_matches!(
            client_end.describe().await.expect("fidl failed"),
            fio::SymlinkInfo {
                target: Some(target),
                ..
            } if target == b"target"
        );
    }

    #[fuchsia::test]
    async fn test_xattrs() {
        let scope = ExecutionScope::new();
        let (client_end, server_end) = create_proxy::<fio::SymlinkMarker>();

        scope.spawn(
            Connection::new(scope.clone(), Arc::new(TestSymlink::new()))
                .run(fio::OpenFlags::RIGHT_READABLE.to_object_request(server_end)),
        );

        client_end
            .set_extended_attribute(
                b"foo",
                fio::ExtendedAttributeValue::Bytes(b"bar".to_vec()),
                fio::SetExtendedAttributeMode::Set,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            client_end.get_extended_attribute(b"foo").await.unwrap().unwrap(),
            fio::ExtendedAttributeValue::Bytes(b"bar".to_vec()),
        );
        let (iterator_client_end, iterator_server_end) =
            create_proxy::<fio::ExtendedAttributeIteratorMarker>();
        client_end.list_extended_attributes(iterator_server_end).unwrap();
        assert_eq!(
            iterator_client_end.get_next().await.unwrap().unwrap(),
            (vec![b"bar".to_vec()], true)
        );
        client_end.remove_extended_attribute(b"foo").await.unwrap().unwrap();
        assert_eq!(
            client_end.get_extended_attribute(b"foo").await.unwrap().unwrap_err(),
            Status::NOT_FOUND.into_raw(),
        );
    }
}
