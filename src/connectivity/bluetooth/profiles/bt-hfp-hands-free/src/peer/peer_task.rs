// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Error, Result};
use fuchsia_async as fasync;
use fuchsia_bluetooth::types::{Channel, PeerId};
use tracing::{info, warn};

use crate::config::HandsFreeFeatureSupport;
use crate::peer::service_level_connection::SlcState;

pub struct PeerTask {
    peer_id: PeerId,
    // TODO(fxb/127025) Use to manage AT responses
    #[allow(unused)]
    slc_state: SlcState,
}

impl PeerTask {
    pub fn spawn(
        peer_id: PeerId,
        config: HandsFreeFeatureSupport,
        _rfcomm: Channel,
    ) -> fasync::Task<()> {
        // TODO(fxr/127086) Use the RFCOMM channel to set up an AT connection
        let slc_state = SlcState::new(config);

        let peer_task = Self { peer_id, slc_state };

        let fasync_task = fasync::Task::local(peer_task.run());
        fasync_task
    }

    pub async fn run(mut self) {
        info!(peer=%self.peer_id, "Starting task.");
        let result = (&mut self).run_inner().await;
        match result {
            Ok(_) => info!(peer=%self.peer_id, "Successfully finished task."),
            Err(err) => warn!(peer = %self.peer_id, error = %err, "Finished task with error"),
        }
    }

    /// Processes and handles received AT responses from the remote peer through the RFCOMM channel
    async fn run_inner(&mut self) -> Result<(), Error> {
        // TODO(fxb/127025) Select on FIDL messages and AT responses
        Ok(())
    }
}
