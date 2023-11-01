// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::{receiver::MessageOrTask, AnyCast, Capability, CloneError},
    fidl::endpoints::{create_request_stream, ServerEnd},
    fidl_fuchsia_component_sandbox as fsandbox, fuchsia_async as fasync,
    fuchsia_zircon::{self as zx, HandleBased},
    futures::{channel::mpsc, future::BoxFuture, FutureExt, TryStreamExt},
    std::fmt::Debug,
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

    pub fn send_handle(&mut self, handle: zx::Handle) {
        self.send_internal(MessageOrTask::Message(M::from(handle)))
    }

    pub fn send_message(&mut self, message: M) {
        self.send_internal(MessageOrTask::Message(message))
    }

    fn send_internal(&mut self, message_or_task: MessageOrTask<M>) {
        // TODO: what lifecycle transitions would cause a receiver to be destroyed and leave a sender?
        self.inner.unbounded_send(message_or_task).expect("Sender has no corresponding Receiver")
    }
}

impl<M: Capability + From<zx::Handle>> Capability for Sender<M> {
    fn try_clone(&self) -> Result<Self, CloneError> {
        Ok(self.clone())
    }

    fn to_zx_handle(self) -> (zx::Handle, Option<BoxFuture<'static, ()>>) {
        let (sender_client_end, sender_stream) =
            create_request_stream::<fsandbox::SenderMarker>().unwrap();
        let mut this = self.clone();
        let task = fasync::Task::spawn(self.serve_sender(sender_stream));
        this.send_internal(MessageOrTask::Task(task));
        (sender_client_end.into_handle(), None)
    }
}

impl<M: Capability + From<zx::Handle>> Sender<M> {
    pub fn serve_sender(self, stream: fsandbox::SenderRequestStream) -> BoxFuture<'static, ()> {
        self.serve_sender_internal(stream).boxed()
    }

    async fn serve_sender_internal(mut self, mut stream: fsandbox::SenderRequestStream) {
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
