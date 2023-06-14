// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Result};
use bt_rfcomm::profile as rfcomm;
use fidl_fuchsia_bluetooth as fidl_bt;
use fidl_fuchsia_bluetooth_bredr as bredr;
use fuchsia_async as fasync;
use fuchsia_bluetooth::profile::ProtocolDescriptor;
use fuchsia_bluetooth::types::{Channel, PeerId};
use profile_client::ProfileEvent;
use tracing::info;

use peer_task::PeerTask;

use crate::config::HandsFreeFeatureSupport;

mod indicators;
mod peer_task;
mod procedure;
mod service_level_connection;

/// Represents a Bluetooth peer that supports the AG role. Manages the Service Level Connection,
/// Audio Connection, and FIDL APIs
pub struct Peer {
    peer_id: PeerId,
    config: HandsFreeFeatureSupport,
    profile_proxy: bredr::ProfileProxy,
    /// The processing task for data received from the remote peer over RFCOMM
    /// or FIDL APIs.
    /// This value is None if there is no RFCOMM channel present.
    /// If set, there is no guarantee that the RFCOMM channel is open.
    task: Option<fasync::Task<()>>,
}

impl Peer {
    pub fn new(
        peer_id: PeerId,
        config: HandsFreeFeatureSupport,
        profile_proxy: bredr::ProfileProxy,
    ) -> Self {
        Self { peer_id, config, profile_proxy, task: None }
    }

    pub async fn handle_profile_event(&mut self, event: ProfileEvent) -> Result<()> {
        match event {
            ProfileEvent::PeerConnected { channel, .. } => {
                info!("Received peer_connected for peer {}; spawning new task.", self.peer_id);
                if self.task.take().is_some() {
                    info!(peer = %self.peer_id, "Shutdown existing task on incoming RFCOMM channel");
                }
                let task = PeerTask::spawn(self.peer_id, self.config, channel);
                self.task = Some(task);
            }
            ProfileEvent::SearchResult { protocol, attributes, .. } => {
                info!(
                    "Received search results for peer {}: {:?}, {:?}",
                    self.peer_id, protocol, attributes
                );
                if !self.task.is_some() {
                    // If we haven't started the task, connect to the peer and do so.
                    info!("Connecting RFCOMM to peer {}", self.peer_id);
                    let protocol =
                        protocol.map(|p| p.iter().map(ProtocolDescriptor::from).collect());
                    let channel = self.connect_from_protocol(protocol).await?;
                    let task = PeerTask::spawn(self.peer_id, self.config, channel);
                    self.task = Some(task);
                }
            }
        }
        Ok(())
    }

    async fn connect_from_protocol(
        &mut self,
        protocol_option: Option<Vec<ProtocolDescriptor>>,
    ) -> Result<Channel> {
        let protocol = protocol_option.ok_or_else(|| {
            format_err!("Got no protocols for peer {:} in search result.", self.peer_id)
        })?;

        let server_channel_option = rfcomm::server_channel_from_protocol(&protocol);
        let server_channel = server_channel_option.ok_or_else(|| {
            format_err!(
                "Search result received for non-RFCOMM protocol {:?} from peer {:}.",
                protocol,
                self.peer_id
            )
        })?;

        let params = bredr::ConnectParameters::Rfcomm(bredr::RfcommParameters {
            channel: Some(server_channel.into()),
            ..bredr::RfcommParameters::default()
        });
        let peer_id: fidl_bt::PeerId = self.peer_id.into();

        let fidl_channel_result_result = self.profile_proxy.connect(&peer_id, &params).await;
        let fidl_channel = fidl_channel_result_result
            .map_err(|e| {
                format_err!(
                    "Unable to connect RFCOMM to peer {:} with FIDL error {:?}",
                    self.peer_id,
                    e
                )
            })?
            .map_err(|e| {
                format_err!(
                    "Unable to connect RFCOMM to peer {:} with Bluetooth error {:?}",
                    self.peer_id,
                    e
                )
            })?;

        // Convert a bredr::Channel into a fuchsia_bluetooth::types::Channel
        let channel = fidl_channel.try_into()?;

        Ok(channel)
    }

    #[cfg(test)]
    fn get_task(&self) -> &Option<fasync::Task<()>> {
        &self.task
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use async_utils::PollExt;
    use fidl_fuchsia_bluetooth_bredr::ProfileMarker;
    use futures::pin_mut;
    use test_profile_server;

    use crate::config::HandsFreeFeatureSupport;

    #[fuchsia::test]
    fn peer_channel_starts_task() {
        let mut exec = fasync::TestExecutor::new();
        let (_near, far) = Channel::create();
        let (profile_proxy, _profile_server) =
            fidl::endpoints::create_proxy_and_stream::<ProfileMarker>().unwrap();
        let event =
            ProfileEvent::PeerConnected { id: PeerId(1), protocol: Vec::new(), channel: far };
        let mut peer =
            Peer::new(PeerId::random(), HandsFreeFeatureSupport::default(), profile_proxy.clone());
        exec.run_singlethreaded(peer.handle_profile_event(event))
            .expect("Error handling profile event.");
        let _: &fasync::Task<()> = peer.get_task().as_ref().expect("Peer task does not exists.");
    }

    #[fuchsia::test]
    fn search_result_connects_channel_and_starts_task() {
        let mut exec = fasync::TestExecutor::new();

        let test_profile_server::TestProfileServerEndpoints {
            proxy: profile_proxy,
            client: _profile_client,
            test_server: mut profile_server,
        } = test_profile_server::TestProfileServer::new(None, None);

        let protocol_descriptor = bredr::ProtocolDescriptor {
            protocol: bredr::ProtocolIdentifier::Rfcomm,
            params: vec![/* Server Channel */ bredr::DataElement::Uint8(1)],
        };

        let event = ProfileEvent::SearchResult {
            id: PeerId(1),
            protocol: Some(vec![protocol_descriptor]),
            attributes: vec![],
        };

        let mut peer =
            Peer::new(PeerId(1), HandsFreeFeatureSupport::default(), profile_proxy.clone());

        {
            let profile_event_fut = peer.handle_profile_event(event);
            pin_mut!(profile_event_fut);
            exec.run_until_stalled(&mut profile_event_fut)
                .expect_pending("Start handling profile event");
            let _near_rfcomm = exec.run_singlethreaded(
                profile_server
                    .expect_connect(Some(test_profile_server::ConnectChannel::RfcommChannel(1))),
            );
            exec.run_singlethreaded(profile_event_fut)
                .expect("Profile event terminated incorrectly.");
        }

        let _: &fasync::Task<()> = peer.get_task().as_ref().expect("Peer task does not exists.");
    }
}
