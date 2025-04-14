// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;

use bt_bap::types::BroadcastId;
use bt_bass::client::error::Error as BassClientError;
use bt_bass::client::BroadcastAudioScanServiceClient;
use bt_common::{PeerId, Uuid};
use bt_gatt::central::*;
use bt_gatt::client::PeerServiceHandle;
use bt_gatt::types::Error as GattError;
use bt_gatt::Client;

pub mod event;
use event::*;
pub mod peer;
pub use peer::Peer;

use crate::types::*;

pub const BROADCAST_AUDIO_SCAN_SERVICE: Uuid = Uuid::from_u16(0x184F);
pub const BASIC_AUDIO_ANNOUNCEMENT_SERVICE: Uuid = Uuid::from_u16(0x1851);
pub const BROADCAST_AUDIO_ANNOUNCEMENT_SERVICE: Uuid = Uuid::from_u16(0x1852);

#[derive(Debug, Error)]
pub enum Error {
    #[error("GATT operation error: {0:?}")]
    Gatt(#[from] GattError),

    #[error("Broadcast Audio Scan Service client error at peer ({0}): {1:?}")]
    BassClient(PeerId, BassClientError),

    #[error("Not connected to Broadcast Audio Scan Service at peer ({0})")]
    NotConnectedToBass(PeerId),

    #[error("Central scanning terminated unexpectedly")]
    CentralScanTerminated,

    #[error("Failed to connect to service ({1}) at peer ({0})")]
    ConnectionFailure(PeerId, Uuid),

    #[error("Broadcast Assistant was already started previously. It cannot be started twice")]
    AlreadyStarted,

    #[error("Failed due to error: {0}")]
    Generic(String),
}

/// Contains information about the currently-known broadcast
/// sources and the peers they were found on
#[derive(Debug)]
pub(crate) struct DiscoveredBroadcastSources(Mutex<HashMap<PeerId, BroadcastSource>>);

impl DiscoveredBroadcastSources {
    /// Creates a shareable instance of `DiscoveredBroadcastSources`.
    pub fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(HashMap::new())))
    }

    /// Merges the broadcast source data with existing broadcast source data.
    /// Returns the copy of the broadcast source data after the merge and
    /// indicates whether it has changed from before or not.
    pub(crate) fn merge_broadcast_source_data(
        &self,
        peer_id: &PeerId,
        data: &BroadcastSource,
    ) -> (BroadcastSource, bool) {
        let mut lock = self.0.lock();
        let source = lock.entry(*peer_id).or_default();
        let before = source.clone();

        source.merge(data);
        let after = source.clone();
        let changed = before != after;

        (after, changed)
    }

    /// Get a BroadcastSource from a peer id.
    fn get_by_peer_id(&self, peer_id: &PeerId) -> Option<BroadcastSource> {
        let lock = self.0.lock();
        lock.get(&peer_id).clone().map(|source| source.clone())
    }

    /// Get a BroadcastSource from associated broadcast id.
    fn get_by_broadcast_id(&self, broadcast_id: &BroadcastId) -> Option<BroadcastSource> {
        let lock = self.0.lock();
        let info = lock.iter().find(|(&_k, &ref v)| v.broadcast_id == Some(*broadcast_id));
        match info {
            Some((&_peer_id, &ref broadcast_source)) => Some(broadcast_source.clone()),
            None => None,
        }
    }
}

pub struct BroadcastAssistant<T: bt_gatt::GattTypes> {
    central: T::Central,
    broadcast_sources: Arc<DiscoveredBroadcastSources>,
    scan_stream: Option<T::ScanResultStream>,
}

impl<T: bt_gatt::GattTypes + 'static> BroadcastAssistant<T> {
    // Creates a broadcast assistant and sets it up to be ready
    // for broadcast source scanning. Clients must use the `start`
    // method to poll the event stream for scan results.
    pub fn new(central: T::Central) -> Self {
        let scan_result_stream = central.scan(&Self::scan_filters());
        Self {
            central,
            broadcast_sources: DiscoveredBroadcastSources::new(),
            scan_stream: Some(scan_result_stream),
        }
    }

    /// List of scan filters for advertisement data Broadcast Assistant should
    /// look for, which are:
    /// - Service data with Broadcast Audio Announcement Service UUID from
    ///   Broadcast Sources (see BAP spec v1.0.1 Section 3.7.2.1 for details)
    // TODO(b/308481381): define filter for finding broadcast sink.
    fn scan_filters() -> Vec<ScanFilter> {
        vec![Filter::HasServiceData(BROADCAST_AUDIO_ANNOUNCEMENT_SERVICE).into()]
    }

    /// Start broadcast assistant. Returns EventStream that the upper layer can
    /// poll. Upper layer can call methods on BroadcastAssistant based on the
    /// events it sees.
    pub fn start(&mut self) -> Result<EventStream<T>, Error> {
        if self.scan_stream.is_none() {
            return Err(Error::AlreadyStarted);
        }
        Ok(EventStream::<T>::new(self.scan_stream.take().unwrap(), self.broadcast_sources.clone()))
    }

    pub fn scan_for_scan_delegators(&mut self) -> T::ScanResultStream {
        // Scan for service data with Broadcast Audio Scan Service UUID to look
        // for Broadcast Sink collocated with the Scan Delegator (see BAP spec v1.0.1
        // Section 3.9.2 for details).
        self.central.scan(&vec![Filter::HasServiceData(BROADCAST_AUDIO_SCAN_SERVICE).into()])
    }

    pub async fn connect_to_scan_delegator(&mut self, peer_id: PeerId) -> Result<Peer<T>, Error>
    where
        <T as bt_gatt::GattTypes>::NotificationStream: std::marker::Send,
    {
        let client = self.central.connect(peer_id).await?;
        let service_handles = client.find_service(BROADCAST_AUDIO_SCAN_SERVICE).await?;

        for handle in service_handles {
            if handle.uuid() != BROADCAST_AUDIO_SCAN_SERVICE || !handle.is_primary() {
                continue;
            }
            let service = handle.connect().await?;
            let bass = BroadcastAudioScanServiceClient::<T>::create(service)
                .await
                .map_err(|e| Error::BassClient(peer_id, e))?;

            let connected_peer =
                Peer::<T>::new(peer_id, client, bass, self.broadcast_sources.clone());
            return Ok(connected_peer);
        }
        Err(Error::ConnectionFailure(peer_id, BROADCAST_AUDIO_SCAN_SERVICE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use futures::{pin_mut, FutureExt};
    use std::task::Poll;

    use bt_bap::types::*;
    use bt_common::core::{AddressType, AdvertisingSetId};
    use bt_gatt::test_utils::{FakeCentral, FakeClient, FakeTypes};

    use crate::assistant::peer::tests::fake_bass_service;

    #[test]
    fn merge_broadcast_source() {
        let discovered = DiscoveredBroadcastSources::new();
        let bid = BroadcastId::try_from(1001).unwrap();
        let (bs, changed) = discovered.merge_broadcast_source_data(
            &PeerId(1001),
            &BroadcastSource::default()
                .with_address([1, 2, 3, 4, 5, 6])
                .with_address_type(AddressType::Public)
                .with_advertising_sid(AdvertisingSetId(1))
                .with_broadcast_id(bid),
        );
        assert!(changed);
        assert_eq!(
            bs,
            BroadcastSource {
                address: Some([1, 2, 3, 4, 5, 6]),
                address_type: Some(AddressType::Public),
                advertising_sid: Some(AdvertisingSetId(1)),
                broadcast_id: Some(bid),
                pa_interval: None,
                endpoint: None,
            }
        );

        let (bs, changed) = discovered.merge_broadcast_source_data(
            &PeerId(1001),
            &BroadcastSource::default().with_address_type(AddressType::Random).with_endpoint(
                BroadcastAudioSourceEndpoint { presentation_delay_ms: 32, big: vec![] },
            ),
        );
        assert!(changed);
        assert_eq!(
            bs,
            BroadcastSource {
                address: Some([1, 2, 3, 4, 5, 6]),
                address_type: Some(AddressType::Random),
                advertising_sid: Some(AdvertisingSetId(1)),
                broadcast_id: Some(bid),
                pa_interval: None,
                endpoint: Some(BroadcastAudioSourceEndpoint {
                    presentation_delay_ms: 32,
                    big: vec![]
                }),
            }
        );

        let (_, changed) = discovered.merge_broadcast_source_data(
            &PeerId(1001),
            &BroadcastSource::default().with_address_type(AddressType::Random).with_endpoint(
                BroadcastAudioSourceEndpoint { presentation_delay_ms: 32, big: vec![] },
            ),
        );
        assert!(!changed);
    }

    #[test]
    fn start_stream() {
        let mut assistant = BroadcastAssistant::<FakeTypes>::new(FakeCentral::new());
        let _ = assistant.start().expect("can start stream");

        // Stream can only be started once.
        assert!(assistant.start().is_err());
    }

    #[test]
    fn connect_to_scan_delegator() {
        // Set up fake GATT related objects.
        let mut central = FakeCentral::new();
        let mut client = FakeClient::new();
        central.add_client(PeerId(1004), client.clone());
        let service = fake_bass_service();
        client.add_service(BROADCAST_AUDIO_SCAN_SERVICE, true, service.clone());

        let mut noop_cx = futures::task::Context::from_waker(futures::task::noop_waker_ref());
        let mut assistant = BroadcastAssistant::<FakeTypes>::new(central);
        let conn_fut = assistant.connect_to_scan_delegator(PeerId(1004));
        pin_mut!(conn_fut);
        let polled = conn_fut.poll_unpin(&mut noop_cx);
        let Poll::Ready(res) = polled else {
            panic!("should be ready");
        };
        let _ = res.expect("should be ok");
    }
}
