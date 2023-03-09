// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::message::action_fuse::ActionFuseHandle;
use crate::message::base::{MessageEvent, Signature, Status};
use crate::message::message_client::MessageClient;
use anyhow::{format_err, Error};
use futures::channel::mpsc::UnboundedReceiver;
use futures::task::{Context, Poll};
use futures::Stream;
use futures::StreamExt;
use std::convert::TryFrom;
use std::pin::Pin;

type EventReceiver = UnboundedReceiver<MessageEvent>;

/// A Receptor is a wrapper around a channel dedicated towards either receiving
/// top-level messages delivered to the recipient's address or replies to a
/// message the recipient sent previously. Receptors are always paired with a
/// Beacon.
///
/// Clients interact with the Receptor similar to a Receiver, waiting on a new
/// MessageEvent via the watch method.
pub struct Receptor {
    signature: Signature,
    event_rx: EventReceiver,
    // Fuse to be triggered when all receptors go out of scope.
    _fuse: ActionFuseHandle,
    _chained_fuse: Option<ActionFuseHandle>,
}

impl Stream for Receptor {
    type Item = MessageEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.event_rx.poll_next_unpin(cx)
    }
}

impl Receptor {
    pub(super) fn new(
        signature: Signature,
        event_rx: EventReceiver,
        fuse: ActionFuseHandle,
        chained_fuse: Option<ActionFuseHandle>,
    ) -> Self {
        Self { signature, event_rx, _fuse: fuse, _chained_fuse: chained_fuse }
    }

    /// Returns the signature associated the top level messenger associated with
    /// this receptor.
    pub(crate) fn get_signature(&self) -> Signature {
        self.signature
    }

    /// Returns the next pending payload, returning an Error if the origin
    /// message (if any) was not deliverable or another error was encountered.
    pub(crate) async fn next_payload(&mut self) -> Result<(crate::Payload, MessageClient), Error> {
        while let Some(event) = self.next().await {
            match event {
                MessageEvent::Message(payload, client) => {
                    return Ok((payload, client));
                }
                MessageEvent::Status(Status::Undeliverable) => {
                    return Err(format_err!("origin message not delivered"));
                }
                _ => {}
            }
        }

        Err(format_err!("could not retrieve payload"))
    }

    pub(crate) async fn next_of<T: TryFrom<crate::Payload>>(
        &mut self,
    ) -> Result<(T, MessageClient), Error>
    where
        <T as std::convert::TryFrom<crate::Payload>>::Error: std::fmt::Debug,
    {
        let (payload, client) = self.next_payload().await?;

        let converted_payload = T::try_from(payload.clone())
            .map(move |converted_payload| (converted_payload, client))
            .map_err(|err| format_err!("conversion failed: {:?}", err));

        // Treat any conversion failures as fatal.
        if converted_payload.is_err() {
            panic!("did not receive payload of expected type {payload:?}");
        }

        converted_payload
    }

    /// Loops until a message of the given type is received ignoring unmatched messages.
    #[cfg(test)]
    pub(crate) async fn next_of_type<T: TryFrom<crate::Payload>>(
        &mut self,
    ) -> Result<(T, MessageClient), Error>
    where
        <T as std::convert::TryFrom<crate::Payload>>::Error: std::fmt::Debug,
    {
        loop {
            let (payload, client) = self.next_payload().await?;
            let converted_payload = T::try_from(payload.clone())
                .map(move |converted_payload| (converted_payload, client))
                .map_err(|err| format_err!("conversion failed: {:?}", err));

            if converted_payload.is_ok() {
                return converted_payload;
            }

            // Just go on to the next message if not matched.
        }
    }

    #[cfg(test)]
    pub(crate) async fn wait_for_acknowledge(&mut self) -> Result<(), Error> {
        while let Some(event) = self.next().await {
            match event {
                MessageEvent::Status(Status::Acknowledged) => {
                    return Ok(());
                }
                MessageEvent::Status(Status::Undeliverable) => {
                    return Err(format_err!("origin message not delivered"));
                }
                _ => {}
            }
        }

        Err(format_err!("did not encounter acknowledged status"))
    }
}

/// Extracts the payload from a given `MessageEvent`. Such event is provided
/// in an optional argument to match the return value from `Receptor` stream.
pub(crate) fn extract_payload(event: Option<MessageEvent>) -> Option<crate::Payload> {
    if let Some(MessageEvent::Message(payload, _)) = event {
        Some(payload)
    } else {
        None
    }
}
