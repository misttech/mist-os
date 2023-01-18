// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::message::base::{
    CreateMessengerResult, MessengerAction, MessengerActionSender, MessengerType, Payload,
    Signature,
};
#[cfg(test)]
use crate::message::base::{MessageError, MessengerPresenceResult};
use crate::message::messenger::Builder;

/// [Delegate] is the artifact of creating a MessageHub. It can be used
/// to create new messengers.
#[derive(Clone)]
pub struct Delegate<P: Payload + 'static> {
    messenger_action_tx: MessengerActionSender<P>,
}

impl<P: Payload + 'static> Delegate<P> {
    pub(super) fn new(action_tx: MessengerActionSender<P>) -> Delegate<P> {
        Delegate { messenger_action_tx: action_tx }
    }

    /// Returns a builder for constructing a new messenger.
    pub(crate) fn messenger_builder(&self, messenger_type: MessengerType<P>) -> Builder<P> {
        Builder::new(self.messenger_action_tx.clone(), messenger_type)
    }

    pub(crate) async fn create(
        &self,
        messenger_type: MessengerType<P>,
    ) -> CreateMessengerResult<P> {
        self.messenger_builder(messenger_type).build().await
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
