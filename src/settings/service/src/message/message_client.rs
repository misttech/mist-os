// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::message::action_fuse::{ActionFuse, ActionFuseHandle};
use crate::message::base::{
    Attribution, Audience, Message, MessageAction, MessageClientId, MessageType, Signature, Status,
};
use crate::message::beacon::BeaconBuilder;
use crate::message::messenger::Messenger;
use crate::message::receptor::Receptor;

/// MessageClient provides a subset of Messenger functionality around a specific
/// delivered message. The client may duplicate/move the MessageClient as
/// desired. Once all MessageClient instances go out of scope, the original
/// message is forwarded to the next Messenger if no interaction preceded it.
#[derive(Clone, Debug)]
pub struct MessageClient {
    // A unique identifier that identifies this client within the parent message
    // hub.
    id: MessageClientId,
    // The "source" message for the client. Any replies or action are done in the
    // context of this message.
    message: Message,
    // The messenger to receive any actions.
    messenger: Messenger,
    // Auto-trigger for automatically forwarding the message to the next
    // recipient.
    forwarder: ActionFuseHandle,
}

impl PartialEq for MessageClient {
    fn eq(&self, other: &MessageClient) -> bool {
        other.id == self.id
    }
}

impl MessageClient {
    pub(super) fn new(
        id: MessageClientId,
        message: Message,
        messenger: Messenger,
    ) -> MessageClient {
        let fuse_messenger_clone = messenger.clone();
        let fuse_message_clone = message.clone();
        MessageClient {
            id,
            message,
            messenger,
            forwarder: ActionFuse::create(Box::new(move || {
                fuse_messenger_clone.forward(fuse_message_clone.clone(), None);
            })),
        }
    }

    #[cfg(test)]
    pub(crate) fn get_modifiers(&self) -> Vec<Signature> {
        self.message.get_modifiers()
    }

    /// Returns the Signature of the original author of the associated Message.
    /// This value can be used to communicate with the author at top-level
    /// communication.
    pub(crate) fn get_author(&self) -> Signature {
        self.message.get_author()
    }

    /// Returns the audience associated with the underlying [`Message`]. If it
    /// is a new [`Message`] (origin), it will be the target audience.
    /// Otherwise it is the author of the reply.
    pub(crate) fn get_audience(&self) -> Audience {
        match self.message.get_type() {
            MessageType::Origin(audience) => audience.clone(),
            MessageType::Reply(message) => Audience::Messenger(message.get_author()),
        }
    }

    /// Creates a dedicated receptor for receiving future communication on this message thread.
    pub(crate) fn spawn_observer(&mut self) -> Receptor {
        let (beacon, receptor) = BeaconBuilder::new(self.messenger.clone()).build();
        self.messenger.forward(self.message.clone(), Some(beacon));
        ActionFuse::defuse(self.forwarder.clone());

        receptor
    }

    /// Sends a reply to this message.
    pub(crate) fn reply(&self, payload: crate::Payload) -> Receptor {
        self.send(payload, Attribution::Source(MessageType::Reply(Box::new(self.message.clone()))))
    }

    /// Propagates a derived message on the path of the original message.
    #[cfg(test)]
    pub(crate) fn propagate(&self, payload: crate::Payload) -> Receptor {
        self.send(
            payload,
            Attribution::Derived(Box::new(self.message.clone()), self.messenger.get_signature()),
        )
    }

    /// Report back to the clients that the message has been acknowledged.
    pub(crate) async fn acknowledge(&self) {
        self.message.report_status(Status::Acknowledged).await;
    }

    /// Tracks the lifetime of the reply listener, firing the fuse when it
    /// goes out of scope.
    pub(crate) async fn bind_to_recipient(&mut self, fuse: ActionFuseHandle) {
        self.message.bind_to_author(fuse).await;
    }

    fn send(&self, payload: crate::Payload, attribution: Attribution) -> Receptor {
        let (beacon, receptor) = BeaconBuilder::new(self.messenger.clone()).build();
        self.messenger.transmit(MessageAction::Send(payload, attribution), Some(beacon));

        ActionFuse::defuse(self.forwarder.clone());

        receptor
    }
}
