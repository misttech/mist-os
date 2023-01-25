// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::message::base::{Attribution, Message, MessageAction, MessageType};
use crate::message::beacon::BeaconBuilder;
use crate::message::messenger::Messenger;
use crate::message::receptor::Receptor;
use fuchsia_zircon::Duration;

/// MessageBuilder allows constructing a message or reply with optional signals.
#[derive(Clone)]
pub struct MessageBuilder {
    payload: crate::Payload,
    attribution: Attribution,
    messenger: Messenger,
    timeout: Option<Duration>,
}

impl MessageBuilder {
    /// Returns a new MessageBuilder. Note that this is private as clients should
    /// retrieve builders through either a Messenger or MessageClient.
    pub(super) fn new(
        payload: crate::Payload,
        message_type: MessageType,
        messenger: Messenger,
    ) -> MessageBuilder {
        MessageBuilder {
            payload,
            attribution: Attribution::Source(message_type),
            messenger,
            timeout: None,
        }
    }

    /// Creates a new message derived from the source message with the specified
    /// payload. Derived messages will follow the same path as the source
    /// message.
    pub(super) fn derive(
        payload: crate::Payload,
        source: Message,
        messenger: Messenger,
    ) -> MessageBuilder {
        MessageBuilder {
            payload,
            attribution: Attribution::Derived(Box::new(source), messenger.get_signature()),
            messenger,
            timeout: None,
        }
    }

    pub(crate) fn set_timeout(mut self, duration: Option<Duration>) -> MessageBuilder {
        self.timeout = duration;
        self
    }

    /// Consumes the MessageBuilder and sends the message to the MessageHub.
    pub(crate) fn send(self) -> Receptor {
        let (beacon, receptor) =
            BeaconBuilder::new(self.messenger.clone()).set_timeout(self.timeout).build();
        self.messenger.transmit(MessageAction::Send(self.payload, self.attribution), Some(beacon));

        receptor
    }
}
