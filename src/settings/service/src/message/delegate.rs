// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::message::base::messenger::Descriptor;
#[cfg(test)]
use crate::message::base::MessengerPresenceResult;
use crate::message::base::{
    CreateMessengerResult, MessageError, MessengerAction, MessengerActionSender, MessengerType,
    Signature,
};

/// [Delegate] is the artifact of creating a MessageHub. It can be used
/// to create new messengers.
#[derive(Clone)]
pub struct Delegate {
    messenger_action_tx: MessengerActionSender,
}

impl Delegate {
    pub(super) fn new(action_tx: MessengerActionSender) -> Delegate {
        Delegate { messenger_action_tx: action_tx }
    }

    pub(crate) async fn create(&self, messenger_type: MessengerType) -> CreateMessengerResult {
        self.create_messenger(messenger_type).await
    }

    #[cfg(test)]
    pub(crate) async fn create_sink(&self) -> CreateMessengerResult {
        self.create_messenger(MessengerType::EventSink).await
    }

    async fn create_messenger(&self, messenger_type: MessengerType) -> CreateMessengerResult {
        let (tx, rx) = futures::channel::oneshot::channel::<CreateMessengerResult>();

        // We expect the receiving side to exist, panic if sending the `Create` message fails.
        self.messenger_action_tx
            .unbounded_send(MessengerAction::Create(
                Descriptor { messenger_type },
                tx,
                self.messenger_action_tx.clone(),
            ))
            .expect("Failed to send MessengerAction::Create message");

        rx.await.map_err(|_| MessageError::Unexpected).and_then(std::convert::identity)
    }

    /// Checks whether a messenger is present at the given [`Signature`]. Note
    /// that there is no guarantee that the messenger at the given [`Signature`]
    /// will not be deleted or created after this function returns.
    #[cfg(test)]
    pub(crate) async fn contains(&self, signature: Signature) -> MessengerPresenceResult {
        let (tx, rx) = futures::channel::oneshot::channel::<MessengerPresenceResult>();
        self.messenger_action_tx
            .unbounded_send(MessengerAction::CheckPresence(signature, tx))
            .unwrap();
        rx.await.unwrap_or(Err(MessageError::Unexpected))
    }

    pub(crate) fn delete(&self, signature: Signature) {
        self.messenger_action_tx
            .unbounded_send(MessengerAction::DeleteBySignature(signature))
            .expect(
                "Delegate::delete, messenger_action_tx failed to send DeleteBySignature messenger\
                 action",
            );
    }
}
