// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{receiver::MessageOrTask, AnyCast, Capability, CloneError, ConversionError, Open},
    fidl::endpoints::{create_request_stream, ServerEnd},
    fidl_fuchsia_component_sandbox as fsandbox, fidl_fuchsia_io as fio, fuchsia_async as fasync,
    fuchsia_zircon::{self as zx, HandleBased},
    futures::{channel::mpsc, future::BoxFuture, FutureExt, TryStreamExt},
    std::any,
    std::fmt::Debug,
    vfs::{directory::entry::DirectoryEntry, execution_scope::ExecutionScope, path::Path, service},
};

/// A capability that represents the sending end of a channel that transfers Zircon handles.
#[derive(Capability, Debug)]
pub struct Sender<M: Capability + From<zx::Handle>> {
    inner: mpsc::UnboundedSender<MessageOrTask<M>>,
}

impl<M: Capability + From<zx::Handle>> Clone for Sender<M> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

impl<M: Capability + From<zx::Handle>> Sender<M> {
    pub(crate) fn new(sender: mpsc::UnboundedSender<MessageOrTask<M>>) -> Self {
        Self { inner: sender }
    }

    pub fn send_handle(&self, handle: zx::Handle) {
        self.send_internal(MessageOrTask::Message(M::from(handle)))
    }

    pub fn send_message(&self, message: M) {
        self.send_internal(MessageOrTask::Message(message))
    }

    fn send_internal(&self, message_or_task: MessageOrTask<M>) {
        // TODO: what lifecycle transitions would cause a receiver to be destroyed and leave a sender?
        self.inner.unbounded_send(message_or_task).expect("Sender has no corresponding Receiver")
    }
}

impl<M: Capability + From<zx::Handle>> Capability for Sender<M> {
    fn try_clone(&self) -> Result<Self, CloneError> {
        Ok(self.clone())
    }

    fn try_into_capability(
        self,
        type_id: any::TypeId,
    ) -> Result<Box<dyn any::Any>, ConversionError> {
        if type_id == any::TypeId::of::<Self>() {
            return Ok(Box::new(self) as Box<dyn any::Any>);
        }
        if type_id == any::TypeId::of::<Open>() {
            return Ok(Box::new(Open::from(self)) as Box<dyn any::Any>);
        }
        Err(ConversionError::NotSupported)
    }

    fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        let (sender_client_end, sender_stream) =
            create_request_stream::<fsandbox::SenderMarker>().unwrap();
        let this = self.clone();
        let task = fasync::Task::spawn(self.serve_sender(sender_stream));
        this.send_internal(MessageOrTask::Task(task));
        (sender_client_end.into_handle(), None)
    }
}

impl<M: Capability + From<zx::Handle>> Sender<M> {
    pub fn serve_sender(self, stream: fsandbox::SenderRequestStream) -> BoxFuture<'static, ()> {
        self.serve_sender_internal(stream).boxed()
    }

    async fn serve_sender_internal(self, mut stream: fsandbox::SenderRequestStream) {
        while let Ok(Some(request)) = stream.try_next().await {
            match request {
                fsandbox::SenderRequest::Send_ { capability, control_handle: _ } => {
                    self.send_handle(capability);
                }
                fsandbox::SenderRequest::Open {
                    flags: _,
                    mode: _,
                    path: _,
                    object,
                    control_handle: _,
                } => {
                    self.send_handle(object.into());
                }
                fsandbox::SenderRequest::Clone2 { request, control_handle: _ } => {
                    let sender = self.clone();
                    let server_end: ServerEnd<fsandbox::SenderMarker> =
                        ServerEnd::new(request.into_channel());
                    let stream = server_end.into_stream().unwrap();
                    let task = fasync::Task::spawn(sender.serve_sender(stream));
                    self.send_internal(MessageOrTask::Task(task));
                }
            }
        }
    }
}

impl<M: Capability + From<zx::Handle>> From<Sender<M>> for Open {
    fn from(value: Sender<M>) -> Open {
        let connect_fn = move |_scope: ExecutionScope, channel: fasync::Channel| {
            value.send_handle(channel.into_zx_channel().into_handle());
        };
        let service = service::endpoint(connect_fn);

        let open_fn = move |scope: ExecutionScope,
                            flags: fio::OpenFlags,
                            path: Path,
                            server_end: zx::Channel| {
            service.clone().open(scope, flags, path, server_end.into());
        };
        Open::new(open_fn, fio::DirentType::Service)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Dict, Handle, Receiver};
    use assert_matches::assert_matches;
    use fidl::endpoints::ClientEnd;
    use futures::StreamExt;
    use zx::AsHandleRef;

    #[fuchsia::test]
    async fn test_into_open() {
        let receiver = Receiver::<Handle>::new();
        let sender = receiver.new_sender();
        let open: Open = sender.into();
        let (client_end, server_end) = zx::Channel::create();
        let scope = ExecutionScope::new();
        open.open(scope, fio::OpenFlags::empty(), ".".to_owned(), server_end);
        let server_end = receiver.receive().await;
        assert_eq!(
            client_end.basic_info().unwrap().related_koid,
            server_end.basic_info().unwrap().koid
        );
    }

    #[test]
    fn test_into_open_extra_path() {
        let mut ex = fasync::TestExecutor::new();

        let receiver = Receiver::<Handle>::new();
        let sender = receiver.new_sender();
        let open: Open = sender.into();
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
    async fn test_into_open_via_dict() {
        let mut dict = Dict::new();
        let receiver = Receiver::<Handle>::new();
        let sender = receiver.new_sender();
        dict.entries.insert("echo".to_owned(), Box::new(sender));

        let open: Open = dict.try_into().unwrap();
        let (client_end, server_end) = zx::Channel::create();
        let scope = ExecutionScope::new();
        open.open(scope, fio::OpenFlags::empty(), "echo".to_owned(), server_end);

        let server_end = receiver.receive().await;
        assert_eq!(
            client_end.basic_info().unwrap().related_koid,
            server_end.basic_info().unwrap().koid
        );
    }

    #[test]
    fn test_into_open_via_dict_extra_path() {
        let mut ex = fasync::TestExecutor::new();

        let mut dict = Dict::new();
        let receiver = Receiver::<Handle>::new();
        let sender = receiver.new_sender();
        dict.entries.insert("echo".to_owned(), Box::new(sender));

        let open: Open = dict.try_into().unwrap();
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
