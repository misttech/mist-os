// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Error, Result};
use at_commands as at;
use fuchsia_async as fasync;
use fuchsia_bluetooth::types::{Channel, PeerId};
use futures::select;
use futures::StreamExt;
use tracing::{debug, info, warn};

use crate::config::HandsFreeFeatureSupport;
use crate::peer::at_connection::AtConnection;
use crate::peer::procedure::{ProcedureInput, ProcedureOutput};
use crate::peer::procedure_manager::ProcedureManager;

pub struct PeerTask {
    peer_id: PeerId,
    procedure_manager: ProcedureManager<ProcedureInput, ProcedureOutput>,
    at_connection: AtConnection,
}

impl PeerTask {
    pub fn spawn(
        peer_id: PeerId,
        config: HandsFreeFeatureSupport,
        rfcomm: Channel,
    ) -> fasync::Task<()> {
        let procedure_manager = ProcedureManager::new(peer_id, config);
        let at_connection = AtConnection::new(peer_id, rfcomm);

        let peer_task = Self { peer_id, procedure_manager, at_connection };

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

    async fn run_inner(&mut self) -> Result<(), Error> {
        select! {
            at_response_result_option = self.at_connection.next() => {
                debug!("Received AT response: {:?}", at_response_result_option);

                let at_response_result =
                    at_response_result_option.ok_or(format_err!("AT connection closed"))?;
                let at_response = at_response_result?;

                self.handle_at_response(at_response);
            }
            procedure_outputs_result_option = self.procedure_manager.next() => {
                debug!("Received procedure output: {:?}", procedure_outputs_result_option);

                let procedure_outputs_result =
                    procedure_outputs_result_option.ok_or(format_err!("Procedure manager stream closed"))?;
                let procedure_outputs = procedure_outputs_result?;

                for procedure_output in procedure_outputs {
                    self.handle_procedure_output(procedure_output);
                }
            }
            // TODO(https://fxbug.dev/127025) Select on FIDL messages and unsolicited messages.
        }
        Ok(())
    }

    fn handle_at_response(&mut self, at_response: at::Response) {
        // TODO(https://fxbug.dev/127362) Handle unsolicited responses separately.

        let procedure_input = ProcedureInput::AtResponseFromAg(at_response);
        self.procedure_manager.enqueue(procedure_input);
    }

    // TODO(https://fxbug.dev/127025) Handle procedure outputs.
    fn handle_procedure_output(&self, _procedure_output: ProcedureOutput) {
        unimplemented!("handle_procedure_output");
    }
}
