// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Result};
use at_commands as at;
use fidl_fuchsia_bluetooth_hfp as fidl_hfp;
use fuchsia_async as fasync;
use fuchsia_bluetooth::types::{Channel, PeerId};
use futures::select;
use futures::StreamExt;
use tracing::{debug, info, warn};

use crate::config::HandsFreeFeatureSupport;
use crate::peer::at_connection::AtConnection;
use crate::peer::procedure::{CommandFromHf, ProcedureInput, ProcedureOutput};
use crate::peer::procedure_manager::ProcedureManager;

pub struct PeerTask {
    peer_id: PeerId,
    procedure_manager: ProcedureManager<ProcedureInput, ProcedureOutput>,
    peer_handler_request_stream: fidl_hfp::PeerHandlerRequestStream,
    at_connection: AtConnection,
}

impl PeerTask {
    pub fn spawn(
        peer_id: PeerId,
        config: HandsFreeFeatureSupport,
        peer_handler_request_stream: fidl_hfp::PeerHandlerRequestStream,
        rfcomm: Channel,
    ) -> fasync::Task<()> {
        let procedure_manager = ProcedureManager::new(peer_id, config);
        let at_connection = AtConnection::new(peer_id, rfcomm);

        let peer_task =
            Self { peer_id, procedure_manager, peer_handler_request_stream, at_connection };

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

    async fn run_inner(&mut self) -> Result<()> {
        select! {
            peer_handler_request_result_option = self.peer_handler_request_stream.next() => {
                debug!("Received FIDL PeerHandler protocol request {:?} from peer {}",
                   peer_handler_request_result_option, self.peer_id);
               let peer_handler_request_result = peer_handler_request_result_option
                   .ok_or(format_err!("FIDL Peer protocol request stream closed for peer {}", self.peer_id))?;
               let peer_handler_request = peer_handler_request_result?;
               self.handle_peer_handler_request(peer_handler_request)?;

            }
            at_response_result_option = self.at_connection.next() => {
                // TODO(fxb/127362) Filter unsolicited AT messages.
                debug!("Received AT response {:?} from peer {}",
                    at_response_result_option, self.peer_id);
                let at_response_result =
                    at_response_result_option
                        .ok_or(format_err!("AT connection stream closed for peer {}", self.peer_id))?;
                let at_response = at_response_result?;

                self.handle_at_response(at_response);
            }
            procedure_outputs_result_option = self.procedure_manager.next() => {
            debug!("Received procedure outputs {:?} for peer {:?}",
                    procedure_outputs_result_option, self.peer_id);

                let procedure_outputs_result =
                    procedure_outputs_result_option
                        .ok_or(format_err!("Procedure manager stream closed for peer {}", self.peer_id))?;
                let procedure_outputs = procedure_outputs_result?;

                for procedure_output in procedure_outputs {
                    self.handle_procedure_output(procedure_output);
                }
            }
        }
        Ok(())
    }

    fn handle_peer_handler_request(
        &mut self,
        peer_handler_request: fidl_hfp::PeerHandlerRequest,
    ) -> Result<()> {
        match peer_handler_request {
            fidl_hfp::PeerHandlerRequest::RequestOutgoingCall {
                action: fidl_hfp::CallAction::DialFromNumber(number),
                responder,
            } => {
                let procedure_input =
                    ProcedureInput::CommandFromHf(CommandFromHf::CallActionDialFromNumber {
                        number,
                    });
                self.procedure_manager.enqueue(procedure_input);
                // TODO(fxbug.dev/42086445) asynchronously respond to this request when the procedure
                // completes.
                let send_result = responder.send(Ok(()));
                if let Err(err) = send_result {
                    warn!("Error {:?} sending result to peer {}", err, self.peer_id);
                }
                ()
            }
            _ => unimplemented!(),
        }

        Ok(())
    }

    fn handle_at_response(&mut self, at_response: at::Response) {
        // TODO(https://fxbug.dev/42077959) Handle unsolicited responses separately.

        let procedure_input = ProcedureInput::AtResponseFromAg(at_response);
        self.procedure_manager.enqueue(procedure_input);
    }

    // TODO(https://fxbug.dev/42077657) Handle procedure outputs.
    fn handle_procedure_output(&self, _procedure_output: ProcedureOutput) {
        unimplemented!("handle_procedure_output");
    }
}
