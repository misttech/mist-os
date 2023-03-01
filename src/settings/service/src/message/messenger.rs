// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::message::action_fuse::ActionFuseHandle;
use crate::message::base::{
    messenger, ActionSender, Attribution, Audience, CreateMessengerResult, Fingerprint, Message,
    MessageAction, MessageError, MessageType, MessengerAction, MessengerActionSender, MessengerId,
    MessengerType, Signature,
};
use crate::message::beacon::{Beacon, BeaconBuilder};
use crate::message::receptor::Receptor;

use fuchsia_syslog::fx_log_warn;
use fuchsia_zircon::Duration;
use std::convert::identity;

/// `Builder` is the default way for creating a new messenger. Beyond the base
/// messenger type, this helper allows for roles to be associated as well
/// during construction.
pub struct Builder {
    /// The sender for sending messenger creation requests to the MessageHub.
    messenger_action_tx: MessengerActionSender,
    /// The type of messenger to be created. Along with roles, the messenger
    /// type determines what audiences the messenger is included in.
    messenger_type: MessengerType,
    /// The optional role to associate with this messenger.
    role: Option<crate::Role>,
}

impl Builder {
    /// Creates a new builder for constructing a messenger of the given
    /// type.
    pub(super) fn new(
        messenger_action_tx: MessengerActionSender,
        messenger_type: MessengerType,
    ) -> Self {
        Self { messenger_action_tx, messenger_type, role: None }
    }

    /// Includes the specified role in the list of roles to be associated with
    /// the new messenger.
    #[cfg(test)]
    pub(crate) fn add_role(mut self, role: crate::Role) -> Self {
        self.role = Some(role);
        self
    }

    /// Constructs a messenger based on specifications supplied.
    pub(crate) async fn build(self) -> CreateMessengerResult {
        let (tx, rx) = futures::channel::oneshot::channel::<CreateMessengerResult>();

        // Panic if send failed since a messenger cannot be created.
        self.messenger_action_tx
            .unbounded_send(MessengerAction::Create(
                messenger::Descriptor { messenger_type: self.messenger_type, role: self.role },
                tx,
                self.messenger_action_tx.clone(),
            ))
            .expect("Builder::build, messenger_action_tx failed to send message");

        rx.await.map_err(|_| MessageError::Unexpected).and_then(identity)
    }
}

/// MessengerClient is a wrapper around a messenger with a fuse.
#[derive(Clone, Debug)]
pub struct MessengerClient {
    messenger: Messenger,
    _fuse: ActionFuseHandle, // Handle that maintains scoped messenger cleanup
}

impl MessengerClient {
    pub(super) fn new(messenger: Messenger, fuse: ActionFuseHandle) -> MessengerClient {
        MessengerClient { messenger, _fuse: fuse }
    }

    /// Creates a MessageBuilder for a new message with the specified payload
    /// and audience.
    pub(crate) fn message(&self, payload: crate::Payload, audience: Audience) -> Receptor {
        self.message_with_timeout(payload, audience, None)
    }

    /// Creates a MessageBuilder for a new message with the specified payload,
    /// audience, and timeout.
    pub(crate) fn message_with_timeout(
        &self,
        payload: crate::Payload,
        audience: Audience,
        duration: Option<Duration>,
    ) -> Receptor {
        let (beacon, receptor) =
            BeaconBuilder::new(self.messenger.clone()).set_timeout(duration).build();
        self.messenger.transmit(
            MessageAction::Send(payload, Attribution::Source(MessageType::Origin(audience))),
            Some(beacon),
        );

        receptor
    }

    /// Returns the signature of the client that will handle any sent messages.
    pub fn get_signature(&self) -> Signature {
        self.messenger.get_signature()
    }
}

/// Messengers provide clients the ability to send messages to other registered
/// clients. They can only be created through a MessageHub.
#[derive(Clone, Debug)]
pub struct Messenger {
    fingerprint: Fingerprint,
    action_tx: ActionSender,
}

impl Messenger {
    pub(super) fn new(fingerprint: Fingerprint, action_tx: ActionSender) -> Messenger {
        Messenger { fingerprint, action_tx }
    }

    /// Returns the identification for this Messenger.
    pub(super) fn get_id(&self) -> MessengerId {
        self.fingerprint.id
    }

    /// Forwards the message to the next Messenger. Note that this method is
    /// private and only called through the MessageClient.
    pub(super) fn forward(&self, message: Message, beacon: Option<Beacon>) {
        self.transmit(MessageAction::Forward(message), beacon);
    }

    /// Tranmits a given action to the message hub. This is a common utility
    /// method to be used for immediate actions (forwarding, observing) and
    /// deferred actions as well (sending, replying).
    pub(super) fn transmit(&self, action: MessageAction, beacon: Option<Beacon>) {
        // Do not transmit if the message hub has exited.
        if self.action_tx.is_closed() {
            return;
        }

        // Log info. transmit is called by forward. However, forward might fail if there is no next
        // Messenger exists.
        self.action_tx.unbounded_send((self.fingerprint, action, beacon)).unwrap_or_else(|_| {
            fx_log_warn!("Messenger::transmit, action_tx failed to send message")
        });
    }

    pub(super) fn get_signature(&self) -> Signature {
        self.fingerprint.signature
    }
}
