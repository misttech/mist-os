// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::VecDeque;
use std::sync::Arc;
use std::task::Poll;

use futures::stream::{BoxStream, FusedStream, SelectAll};
use futures::{Stream, StreamExt};
use parking_lot::Mutex;

use bt_bap::types::BroadcastId;
use bt_common::packet_encoding::Decodable;
use bt_gatt::client::CharacteristicNotification;
use bt_gatt::types::Error as BtGattError;

use crate::client::error::Error;
use crate::client::error::ServiceError;
use crate::client::KnownBroadcastSources;
use crate::types::*;

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    // Broadcast Audio Scan Service (BASS) server requested for SyncInfo through PAST procedure.
    SyncInfoRequested(BroadcastId),
    // BASS server failed to synchornize to PA or did not synchronize to PA.
    NotSyncedToPa(BroadcastId),
    // BASS server successfully synced to PA.
    SyncedToPa(BroadcastId),
    // BASS server failed to sync to PA since SyncInfo wasn't received.
    SyncedFailedNoPast(BroadcastId),
    // BASS server requires code to since the BIS is encrypted.
    BroadcastCodeRequired(BroadcastId),
    // BASS server failed to decrypt BIS using the previously provided code.
    InvalidBroadcastCode(BroadcastId, [u8; 16]),
    // BASS server has autonomously synchronized to a BIS that is encrypted, and the server
    // has the correct encryption key to decrypt the BIS.
    Decrypting(BroadcastId),
    // Received a packet from the BASS server not recognized by this library.
    UnknownPacket,
    // Broadcast source was removed by the BASS server.
    RemovedBroadcastSource(BroadcastId),
    // Broadcast source was added by the BASS server.
    AddedBroadcastSource(BroadcastId, PaSyncState, EncryptionStatus),
}

impl Event {
    pub(crate) fn from_broadcast_receive_state(state: &ReceiveState) -> Vec<Event> {
        let mut events = Vec::new();
        let pa_sync_state = state.pa_sync_state();
        let broadcast_id = state.broadcast_id();
        match pa_sync_state {
            PaSyncState::SyncInfoRequest => events.push(Event::SyncInfoRequested(broadcast_id)),
            PaSyncState::Synced => events.push(Event::SyncedToPa(broadcast_id)),
            PaSyncState::FailedToSync | PaSyncState::NotSynced => {
                events.push(Event::NotSyncedToPa(broadcast_id))
            }
            PaSyncState::NoPast => events.push(Event::SyncedFailedNoPast(broadcast_id)),
        }
        match state.big_encryption() {
            EncryptionStatus::BroadcastCodeRequired => {
                events.push(Event::BroadcastCodeRequired(broadcast_id))
            }
            EncryptionStatus::Decrypting => events.push(Event::Decrypting(broadcast_id)),
            EncryptionStatus::BadCode(code) => {
                events.push(Event::InvalidBroadcastCode(broadcast_id, code.clone()))
            }
            _ => {}
        };
        events
    }
}

/// Trait for representing a stream that outputs Events from BASS. If there was
/// an error the stream should output error instead and terminate.

pub struct EventStream {
    // Actual GATT notification streams that we poll from.
    notification_streams:
        SelectAll<BoxStream<'static, Result<CharacteristicNotification, BtGattError>>>,

    event_queue: VecDeque<Result<Event, Error>>,
    terminated: bool,

    // States to be updated.
    broadcast_sources: Arc<Mutex<KnownBroadcastSources>>,
}

impl EventStream {
    pub(crate) fn new(
        notification_streams: SelectAll<
            BoxStream<'static, Result<CharacteristicNotification, BtGattError>>,
        >,
        broadcast_sources: Arc<Mutex<KnownBroadcastSources>>,
    ) -> Self {
        Self {
            notification_streams,
            event_queue: VecDeque::new(),
            terminated: false,
            broadcast_sources,
        }
    }
}

impl FusedStream for EventStream {
    fn is_terminated(&self) -> bool {
        self.terminated
    }
}

impl Stream for EventStream {
    type Item = Result<Event, Error>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        if self.terminated {
            return Poll::Ready(None);
        }

        loop {
            if let Some(item) = self.event_queue.pop_front() {
                match item {
                    Ok(event) => return Poll::Ready(Some(Ok(event))),
                    Err(e) => {
                        // If an error was received, we terminate the event stream, but send an
                        // error to indicate why it was terminated.
                        self.terminated = true;
                        return Poll::Ready(Some(Err(e)));
                    }
                }
            }

            match self.notification_streams.poll_next_unpin(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => {
                    let err = Error::EventStream(Box::new(Error::Service(
                        ServiceError::NotificationChannelClosed(format!(
                            "GATT notification stream for BRS characteristics closed"
                        )),
                    )));
                    self.event_queue.push_back(Err(err));
                }
                Poll::Ready(Some(Err(error))) => {
                    // Deem all errors as critical.
                    let err = Error::EventStream(Box::new(Error::Gatt(error)));
                    self.event_queue.push_back(Err(err));
                }
                Poll::Ready(Some(Ok(notification))) => {
                    let char_handle = notification.handle;
                    let Ok((new_state, _)) =
                        BroadcastReceiveState::decode(notification.value.as_slice())
                    else {
                        self.event_queue.push_back(Ok(Event::UnknownPacket));
                        continue;
                    };

                    let maybe_prev_state = {
                        let mut lock = self.broadcast_sources.lock();
                        lock.update_state(char_handle, new_state.clone())
                    };

                    let mut multi_events = VecDeque::new();

                    // If the previous value was not empty, check if it was overwritten.
                    if let Some(ref prev_state) = maybe_prev_state {
                        if let BroadcastReceiveState::NonEmpty(prev_receive_state) = prev_state {
                            if new_state.is_empty() || !new_state.has_same_broadcast_id(&prev_state)
                            {
                                multi_events.push_back(Ok(Event::RemovedBroadcastSource(
                                    prev_receive_state.broadcast_id,
                                )));
                            }
                        }
                    }

                    // BRS characteristic value was updated with a new broadcast source
                    // information.
                    if let BroadcastReceiveState::NonEmpty(receive_state) = &new_state {
                        let is_new_source = match maybe_prev_state {
                            Some(prev_state) => !new_state.has_same_broadcast_id(&prev_state),
                            None => true,
                        };
                        if is_new_source {
                            multi_events.push_back(Ok(Event::AddedBroadcastSource(
                                receive_state.broadcast_id,
                                receive_state.pa_sync_state,
                                receive_state.big_encryption,
                            )));
                        } else {
                            let other_events = Event::from_broadcast_receive_state(receive_state);
                            for e in other_events.into_iter() {
                                multi_events.push_back(Ok(e));
                            }
                        }
                    }
                    if multi_events.len() != 0 {
                        self.event_queue.append(&mut multi_events);
                        continue;
                    }
                    continue;
                }
            };

            break;
        }
        Poll::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;

    use assert_matches::assert_matches;
    use futures::channel::mpsc::unbounded;

    use bt_common::core::AddressType;
    use bt_gatt::types::Handle;

    #[test]
    fn poll_event_stream() {
        let mut streams = SelectAll::new();
        let (sender1, receiver1) = unbounded();
        let (sender2, receiver2) = unbounded();
        streams.push(receiver1.boxed());
        streams.push(receiver2.boxed());

        let source_tracker = Arc::new(Mutex::new(KnownBroadcastSources::new(HashMap::from([
            (Handle(0x1), BroadcastReceiveState::Empty),
            (Handle(0x2), BroadcastReceiveState::Empty),
        ]))));
        let mut event_streams = EventStream::new(streams, source_tracker);

        // Send notifications to underlying streams.
        let bad_code_status =
            EncryptionStatus::BadCode([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
        #[rustfmt::skip]
        sender1
            .unbounded_send(Ok(CharacteristicNotification {
                handle: Handle(0x1),
                value: vec![
                    0x01, AddressType::Public as u8,         // source id and address type
                    0x02, 0x03, 0x04, 0x05, 0x06, 0x07,      // address
                    0x01, 0x01, 0x02, 0x03,                  // ad set id and broadcast id
                    PaSyncState::FailedToSync as u8,
                    bad_code_status.raw_value(),
                    1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,  // bad code
                    0x00,                                    // no subgroups
                ],
                maybe_truncated: false,
            }))
            .expect("should send");

        #[rustfmt::skip]
        sender2
            .unbounded_send(Ok(CharacteristicNotification {
                handle: Handle(0x2),
                value: vec![
                    0x02, AddressType::Public as u8,             // source id and address type
                    0x03, 0x04, 0x05, 0x06, 0x07, 0x08,          // address
                    0x01, 0x02, 0x03, 0x04,                      // ad set id and broadcast id
                    PaSyncState::NoPast as u8,
                    EncryptionStatus::NotEncrypted.raw_value(),
                    0x00,                                        // no subgroups
                ],
                maybe_truncated: false,
            }))
            .expect("should send");

        // Events should have been generated from notifications.
        let mut noop_cx = futures::task::Context::from_waker(futures::task::noop_waker_ref());
        let polled = event_streams.poll_next_unpin(&mut noop_cx);
        assert_matches!(polled, Poll::Ready(Some(Ok(event))) => {
            assert_eq!(event, Event::AddedBroadcastSource(BroadcastId::try_from(0x030201).unwrap(), PaSyncState::FailedToSync, EncryptionStatus::BadCode([1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16])));
        });

        let polled = event_streams.poll_next_unpin(&mut noop_cx);
        assert_matches!(polled, Poll::Ready(Some(Ok(event))) => {
            assert_eq!(event, Event::AddedBroadcastSource(BroadcastId::try_from(0x040302).unwrap(), PaSyncState::NoPast, EncryptionStatus::NotEncrypted));
        });

        // Should be pending because no more events generated from notifications.
        assert!(event_streams.poll_next_unpin(&mut noop_cx).is_pending());

        // Send notifications to underlying streams.
        #[rustfmt::skip]
        sender2
            .unbounded_send(Ok(CharacteristicNotification {
                handle: Handle(0x2),
                value: vec![
                    0x02, AddressType::Public as u8,             // source id and address type
                    0x03, 0x04, 0x05, 0x06, 0x07, 0x08,          // address
                    0x01, 0x02, 0x03, 0x04,                      // ad set id and broadcast id
                    PaSyncState::Synced as u8,
                    EncryptionStatus::NotEncrypted.raw_value(),
                    0x00,                                        // no subgroups
                ],
                maybe_truncated: false,
            }))
            .expect("should send");

        // Event should have been generated from notification.
        let mut noop_cx = futures::task::Context::from_waker(futures::task::noop_waker_ref());
        assert_matches!(event_streams.poll_next_unpin(&mut noop_cx), Poll::Ready(Some(Ok(event))) => { assert_eq!(event, Event::SyncedToPa(BroadcastId::try_from(0x040302).unwrap())) });

        // Should be pending because no more events generated from notifications.
        assert!(event_streams.poll_next_unpin(&mut noop_cx).is_pending());
    }

    #[test]
    fn broadcast_source_is_removed() {
        let mut streams = SelectAll::new();
        let (_sender1, receiver1) = unbounded();
        let (sender2, receiver2) = unbounded();
        streams.push(receiver1.boxed());
        streams.push(receiver2.boxed());

        let source_tracker = Arc::new(Mutex::new(KnownBroadcastSources::new(HashMap::from([
            (Handle(0x1), BroadcastReceiveState::Empty),
            (Handle(0x2), BroadcastReceiveState::Empty),
        ]))));
        let mut event_streams = EventStream::new(streams, source_tracker);

        // Send notifications to underlying streams.
        #[rustfmt::skip]
        sender2
            .unbounded_send(Ok(CharacteristicNotification {
                handle: Handle(0x2),
                value: vec![
                    0x02, AddressType::Public as u8,             // source id and address type
                    0x03, 0x04, 0x05, 0x06, 0x07, 0x08,          // address
                    0x01, 0x02, 0x03, 0x04,                      // ad set id and broadcast id
                    PaSyncState::Synced as u8,
                    EncryptionStatus::NotEncrypted.raw_value(),
                    0x00,                                        // no subgroups
                ],
                maybe_truncated: false,
            }))
            .expect("should send");

        // Events should have been generated from notifications.
        let mut noop_cx = futures::task::Context::from_waker(futures::task::noop_waker_ref());

        let polled = event_streams.poll_next_unpin(&mut noop_cx);
        assert_matches!(polled, Poll::Ready(Some(Ok(event))) => {
            assert_eq!(event, Event::AddedBroadcastSource(BroadcastId::try_from(0x040302).unwrap(), PaSyncState::Synced, EncryptionStatus::NotEncrypted));
        });

        // Should be pending because no more events generated from notifications.
        assert!(event_streams.poll_next_unpin(&mut noop_cx).is_pending());

        // Send notifications to underlying streams. Ths time, send empty BRS
        // characteristic value.
        sender2
            .unbounded_send(Ok(CharacteristicNotification {
                handle: Handle(0x2),
                value: vec![],
                maybe_truncated: false,
            }))
            .expect("should send");

        // Event should have been generated from notification.
        let mut noop_cx = futures::task::Context::from_waker(futures::task::noop_waker_ref());
        assert_matches!(event_streams.poll_next_unpin(&mut noop_cx), Poll::Ready(Some(Ok(event))) => { assert_eq!(event, Event::RemovedBroadcastSource(BroadcastId::try_from(0x040302).unwrap())) });

        // Should be pending because no more events generated from notifications.
        assert!(event_streams.poll_next_unpin(&mut noop_cx).is_pending());
    }
}
