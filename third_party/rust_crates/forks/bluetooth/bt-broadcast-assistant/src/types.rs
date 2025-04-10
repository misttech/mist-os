// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use bt_bap::types::*;
use bt_bass::client::BigToBisSync;
use bt_bass::types::{BigSubgroup, BisSync};
use bt_common::core::{Address, AddressType};
use bt_common::core::{AdvertisingSetId, PaInterval};
use bt_common::packet_encoding::Error as PacketError;

/// Broadcast source data as advertised through Basic Audio Announcement
/// PA and Broadcast Audio Announcement.
/// See BAP spec v1.0.1 Section 3.7.2.1 and Section 3.7.2.2 for details.
// TODO(b/308481381): fill out endpoint from basic audio announcement from PA trains.
#[derive(Clone, Default, Debug, PartialEq)]
pub struct BroadcastSource {
    pub(crate) address: Option<Address>,
    pub(crate) address_type: Option<AddressType>,
    pub(crate) advertising_sid: Option<AdvertisingSetId>,
    pub(crate) broadcast_id: Option<BroadcastId>,
    pub(crate) pa_interval: Option<PaInterval>,
    pub(crate) endpoint: Option<BroadcastAudioSourceEndpoint>,
}

impl BroadcastSource {
    /// Returns whether or not this BroadcastSource has enough information
    /// to be added by the Broadcast Assistant.
    pub(crate) fn into_add_source(&self) -> bool {
        // PA interval is not necessary since default value can be used.
        self.address.is_some()
            && self.address_type.is_some()
            && self.advertising_sid.is_some()
            && self.broadcast_id.is_some()
            && self.endpoint.is_some()
    }

    pub fn with_address(&mut self, address: [u8; 6]) -> &mut Self {
        self.address = Some(address);
        self
    }

    pub fn with_address_type(&mut self, type_: AddressType) -> &mut Self {
        self.address_type = Some(type_);
        self
    }

    pub fn with_broadcast_id(&mut self, bid: BroadcastId) -> &mut Self {
        self.broadcast_id = Some(bid);
        self
    }

    pub fn with_advertising_sid(&mut self, sid: AdvertisingSetId) -> &mut Self {
        self.advertising_sid = Some(sid);
        self
    }

    pub fn with_endpoint(&mut self, endpoint: BroadcastAudioSourceEndpoint) -> &mut Self {
        self.endpoint = Some(endpoint);
        self
    }

    /// Merge fields from other broadcast source into this broadcast source.
    /// Set fields in other source take priority over this source.
    /// If a field in the other broadcast source is none, it's ignored and
    /// the existing values are kept.
    pub(crate) fn merge(&mut self, other: &BroadcastSource) {
        if let Some(address) = other.address {
            self.address = Some(address);
        }
        if let Some(address_type) = other.address_type {
            self.address_type = Some(address_type);
        }
        if let Some(advertising_sid) = other.advertising_sid {
            self.advertising_sid = Some(advertising_sid);
        }
        if let Some(broadcast_id) = other.broadcast_id {
            self.broadcast_id = Some(broadcast_id);
        }
        if let Some(pa_interval) = other.pa_interval {
            self.pa_interval = Some(pa_interval);
        }
        if let Some(endpoint) = &other.endpoint {
            self.endpoint = Some(endpoint.clone());
        }
    }

    /// Returns the representation of this object's endpoint field to
    /// broadcast isochronous groups presetation that's usable with
    /// Broadcast Audio Scan Service operations.
    ///
    /// # Arguments
    ///
    /// * `bis_sync` - BIG to BIS sync information. If the set is empty, no
    ///   preference value is used for all the BIGs
    pub(crate) fn endpoint_to_big_subgroups(
        &self,
        bis_sync: BigToBisSync,
    ) -> Result<Vec<BigSubgroup>, PacketError> {
        if self.endpoint.is_none() {
            return Err(PacketError::InvalidParameter(
                "cannot convert empty Broadcast Audio Source Endpoint data to BIG subgroups data"
                    .to_string(),
            ));
        }
        let mut subgroups = Vec::new();
        let sync_map = bt_bass::client::big_to_bis_sync_indices(&bis_sync);

        for (big_index, group) in self.endpoint.as_ref().unwrap().big.iter().enumerate() {
            let bis_sync = match sync_map.get(&(big_index as u8)) {
                Some(bis_indices) => {
                    let mut bis_sync: BisSync = BisSync(0);
                    bis_sync.set_sync(bis_indices)?;
                    bis_sync
                }
                _ => BisSync::default(),
            };
            subgroups.push(BigSubgroup::new(Some(bis_sync)).with_metadata(group.metadata.clone()));
        }
        Ok(subgroups)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashSet;

    use bt_common::core::CodecId;
    use bt_common::generic_audio::metadata_ltv::Metadata;

    #[test]
    fn broadcast_source() {
        let mut b = BroadcastSource::default();
        assert!(!b.into_add_source());

        b.merge(
            &BroadcastSource::default()
                .with_address([0x1, 0x2, 0x3, 0x4, 0x5, 0x6])
                .with_address_type(AddressType::Public)
                .with_advertising_sid(AdvertisingSetId(0x1))
                .with_broadcast_id(BroadcastId::try_from(0x010203).unwrap()),
        );

        assert!(!b.into_add_source());
        b.endpoint_to_big_subgroups(HashSet::from([(0, 1)]))
            .expect_err("should fail no endpoint data");

        b.merge(&BroadcastSource::default().with_endpoint(BroadcastAudioSourceEndpoint {
            presentation_delay_ms: 0x010203,
            big: vec![BroadcastIsochronousGroup {
                codec_id: CodecId::Assigned(bt_common::core::CodingFormat::Cvsd),
                codec_specific_configs: vec![],
                metadata: vec![Metadata::BroadcastAudioImmediateRenderingFlag],
                bis: vec![BroadcastIsochronousStream {
                    bis_index: 1,
                    codec_specific_config: vec![],
                }],
            }],
        }));

        assert!(b.into_add_source());
        let subgroups =
            b.endpoint_to_big_subgroups(HashSet::from([(0, 1), (1, 1)])).expect("should succeed");
        assert_eq!(subgroups.len(), 1);
        assert_eq!(
            subgroups[0],
            BigSubgroup::new(Some(BisSync(0x00000001)))
                .with_metadata(vec![Metadata::BroadcastAudioImmediateRenderingFlag])
        );
    }
}
