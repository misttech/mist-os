// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use futures::stream::FusedStream;
use futures::Stream;
use std::sync::Arc;
use thiserror::Error;

use bt_bap::types::BroadcastId;
use bt_bass::client::error::Error as BassClientError;
use bt_bass::client::event::Event as BassEvent;
use bt_bass::client::{BigToBisSync, BroadcastAudioScanServiceClient};
use bt_bass::types::PaSync;
use bt_common::core::PaInterval;
use bt_common::packet_encoding::Error as PacketError;
use bt_common::PeerId;

use crate::assistant::DiscoveredBroadcastSources;

#[derive(Debug, Error)]
pub enum Error {
    #[error("Failed to take event stream at Broadcast Audio Scan Service client")]
    UnavailableBassEventStream,

    #[error("Broadcast Audio Scan Service client error: {0:?}")]
    BassClient(#[from] BassClientError),

    #[error("Incomplete information for broadcast source with peer id ({0})")]
    NotEnoughInfo(PeerId),

    #[error("Broadcast source with peer id ({0}) does not exist")]
    DoesNotExist(PeerId),

    #[error("Packet error: {0}")]
    PacketError(#[from] PacketError),
}

/// Connected scan delegator peer. Clients can use this
/// object to perform Broadcast Audio Scan Service operations on the
/// scan delegator peer.
/// Not thread-safe and only one operation must be done at a time.
pub struct Peer<T: bt_gatt::GattTypes> {
    peer_id: PeerId,
    // Keep for peer connection.
    _client: T::Client,
    bass: BroadcastAudioScanServiceClient<T>,
    // TODO(b/309015071): add a field for pacs.
    broadcast_sources: Arc<DiscoveredBroadcastSources>,
}

impl<T: bt_gatt::GattTypes> Peer<T> {
    pub(crate) fn new(
        peer_id: PeerId,
        client: T::Client,
        bass: BroadcastAudioScanServiceClient<T>,
        broadcast_sources: Arc<DiscoveredBroadcastSources>,
    ) -> Self {
        Peer { peer_id, _client: client, bass, broadcast_sources }
    }

    pub fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    /// Takes event stream for BASS events from this scan delegator peer.
    /// Clients can call this method to start subscribing to BASS events.
    pub fn take_event_stream(
        &mut self,
    ) -> Result<impl Stream<Item = Result<BassEvent, BassClientError>> + FusedStream, Error> {
        self.bass.take_event_stream().ok_or(Error::UnavailableBassEventStream)
    }

    /// Send broadcast code for a particular broadcast.
    pub async fn send_broadcast_code(
        &mut self,
        broadcast_id: BroadcastId,
        broadcast_code: [u8; 16],
    ) -> Result<(), Error> {
        self.bass.set_broadcast_code(broadcast_id, broadcast_code).await.map_err(Into::into)
    }

    /// Sends a command to add a particular broadcast source.
    ///
    /// # Arguments
    ///
    /// * `broadcast_source_pid` - peer id of the braodcast source that's to be
    ///   added to this scan delegator peer
    /// * `pa_sync` - pa sync mode the peer should attempt to be in
    /// * `bis_sync` - desired BIG to BIS synchronization information. If the
    ///   set is empty, no preference value is used for all the BIGs
    pub async fn add_broadcast_source(
        &mut self,
        source_peer_id: PeerId,
        pa_sync: PaSync,
        bis_sync: BigToBisSync,
    ) -> Result<(), Error> {
        let broadcast_source = self
            .broadcast_sources
            .get_by_peer_id(&source_peer_id)
            .ok_or(Error::DoesNotExist(source_peer_id))?;
        if !broadcast_source.into_add_source() {
            return Err(Error::NotEnoughInfo(source_peer_id));
        }

        self.bass
            .add_broadcast_source(
                broadcast_source.broadcast_id.unwrap(),
                broadcast_source.address_type.unwrap(),
                broadcast_source.address.unwrap(),
                broadcast_source.advertising_sid.unwrap(),
                pa_sync,
                broadcast_source.pa_interval.unwrap_or(PaInterval::unknown()),
                broadcast_source.endpoint_to_big_subgroups(bis_sync).map_err(Error::PacketError)?,
            )
            .await
            .map_err(Into::into)
    }

    /// Sends a command to to update a particular broadcast source's PA sync.
    ///
    /// # Arguments
    ///
    /// * `broadcast_id` - broadcast id of the broadcast source that's to be
    ///   updated
    /// * `pa_sync` - pa sync mode the scan delegator peer should attempt to be
    ///   in.
    /// * `bis_sync` - desired BIG to BIS synchronization information
    pub async fn update_broadcast_source_sync(
        &mut self,
        broadcast_id: BroadcastId,
        pa_sync: PaSync,
        bis_sync: BigToBisSync,
    ) -> Result<(), Error> {
        let pa_interval = self
            .broadcast_sources
            .get_by_broadcast_id(&broadcast_id)
            .map(|bs| bs.pa_interval)
            .unwrap_or(None);

        self.bass
            .modify_broadcast_source(broadcast_id, pa_sync, pa_interval, Some(bis_sync), None)
            .await
            .map_err(Into::into)
    }

    /// Sends a command to remove a particular broadcast source.
    ///
    /// # Arguments
    ///
    /// * `broadcast_id` - broadcast id of the braodcast source that's to be
    ///   removed from the scan delegator
    pub async fn remove_broadcast_source(
        &mut self,
        broadcast_id: BroadcastId,
    ) -> Result<(), Error> {
        self.bass.remove_broadcast_source(broadcast_id).await.map_err(Into::into)
    }

    /// Sends a command to inform the scan delegator peer that we have
    /// started scanning for broadcast sources on behalf of it.
    pub async fn inform_remote_scan_started(&mut self) -> Result<(), Error> {
        self.bass.remote_scan_started().await.map_err(Into::into)
    }

    /// Sends a command to inform the scan delegator peer that we have
    /// stopped scanning for broadcast sources on behalf of it.
    pub async fn inform_remote_scan_stopped(&mut self) -> Result<(), Error> {
        self.bass.remote_scan_stopped().await.map_err(Into::into)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use assert_matches::assert_matches;
    use futures::{pin_mut, FutureExt};
    use std::collections::HashSet;
    use std::task::Poll;

    use bt_common::core::{AddressType, AdvertisingSetId};
    use bt_gatt::test_utils::{FakeClient, FakePeerService, FakeTypes};
    use bt_gatt::types::{
        AttributePermissions, CharacteristicProperties, CharacteristicProperty, Handle,
    };

    use bt_gatt::Characteristic;

    use crate::types::BroadcastSource;

    const RECEIVE_STATE_HANDLE: Handle = Handle(0x11);
    const AUDIO_SCAN_CONTROL_POINT_HANDLE: Handle = Handle(0x12);

    pub(crate) fn fake_bass_service() -> FakePeerService {
        let mut peer_service = FakePeerService::new();
        // One broadcast receive state and one broadcast audio scan control
        // point characteristic handles.
        peer_service.add_characteristic(
            Characteristic {
                handle: RECEIVE_STATE_HANDLE,
                uuid: bt_bass::types::BROADCAST_RECEIVE_STATE_UUID,
                properties: CharacteristicProperties(vec![
                    CharacteristicProperty::Broadcast,
                    CharacteristicProperty::Notify,
                ]),
                permissions: AttributePermissions::default(),
                descriptors: vec![],
            },
            vec![],
        );
        peer_service.add_characteristic(
            Characteristic {
                handle: AUDIO_SCAN_CONTROL_POINT_HANDLE,
                uuid: bt_bass::types::BROADCAST_AUDIO_SCAN_CONTROL_POINT_UUID,
                properties: CharacteristicProperties(vec![CharacteristicProperty::Broadcast]),
                permissions: AttributePermissions::default(),
                descriptors: vec![],
            },
            vec![],
        );
        peer_service
    }

    fn setup() -> (Peer<FakeTypes>, FakePeerService, Arc<DiscoveredBroadcastSources>) {
        let peer_service = fake_bass_service();

        let broadcast_sources = DiscoveredBroadcastSources::new();
        (
            Peer {
                peer_id: PeerId(0x1),
                _client: FakeClient::new(),
                bass: BroadcastAudioScanServiceClient::<FakeTypes>::create_for_test(
                    peer_service.clone(),
                    Handle(0x1),
                ),
                broadcast_sources: broadcast_sources.clone(),
            },
            peer_service,
            broadcast_sources,
        )
    }

    #[test]
    fn take_event_stream() {
        let (mut peer, _peer_service, _broadcast_source) = setup();
        let _event_stream = peer.take_event_stream().expect("should succeed");

        // If we try to take the event stream the second time, it should fail.
        assert!(peer.take_event_stream().is_err());
    }

    #[test]
    fn add_broadcast_source_fail() {
        let (mut peer, _peer_service, broadcast_source) = setup();

        let mut noop_cx = futures::task::Context::from_waker(futures::task::noop_waker_ref());

        // Should fail because broadcast source doesn't exist.
        {
            let fut = peer.add_broadcast_source(
                PeerId(1001),
                PaSync::SyncPastUnavailable,
                HashSet::new(),
            );
            pin_mut!(fut);
            let polled = fut.poll_unpin(&mut noop_cx);
            assert_matches!(polled, Poll::Ready(Err(_)));
        }

        let _ = broadcast_source.merge_broadcast_source_data(
            &PeerId(1001),
            &BroadcastSource::default()
                .with_address([1, 2, 3, 4, 5, 6])
                .with_address_type(AddressType::Public)
                .with_advertising_sid(AdvertisingSetId(1))
                .with_broadcast_id(BroadcastId::try_from(1001).unwrap()),
        );

        // Should fail because not enough information.
        {
            let fut = peer.add_broadcast_source(
                PeerId(1001),
                PaSync::SyncPastUnavailable,
                HashSet::new(),
            );
            pin_mut!(fut);
            let polled = fut.poll_unpin(&mut noop_cx);
            assert_matches!(polled, Poll::Ready(Err(_)));
        }
    }
}
