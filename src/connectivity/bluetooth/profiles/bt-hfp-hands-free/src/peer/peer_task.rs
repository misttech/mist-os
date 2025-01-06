// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Result};
use fuchsia_bluetooth::types::{Channel, PeerId};
use futures::{select, StreamExt};
use log::{debug, info, warn};
use {at_commands as at, fidl_fuchsia_bluetooth_hfp as fidl_hfp, fuchsia_async as fasync};

use crate::config::HandsFreeFeatureSupport;
use crate::peer::ag_indicators::AgIndicatorTranslator;
use crate::peer::at_connection::AtConnection;
use crate::peer::procedure::{CommandFromHf, CommandToHf, ProcedureInput, ProcedureOutput};
use crate::peer::procedure_manager::ProcedureManager;

pub struct PeerTask {
    peer_id: PeerId,
    procedure_manager: ProcedureManager<ProcedureInput, ProcedureOutput>,
    peer_handler_request_stream: fidl_hfp::PeerHandlerRequestStream,
    at_connection: AtConnection,
    ag_indicator_translator: AgIndicatorTranslator,
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
        let ag_indicator_translator = AgIndicatorTranslator::new();

        let peer_task = Self {
            peer_id,
            procedure_manager,
            peer_handler_request_stream,
            at_connection,
            ag_indicator_translator,
        };

        let fasync_task = fasync::Task::local(peer_task.run());
        fasync_task
    }

    pub async fn run(mut self) {
        info!(peer:% = self.peer_id; "Starting task.");
        let result = (&mut self).run_inner().await;
        match result {
            Ok(_) => info!(peer:% =self.peer_id; "Successfully finished task."),
            Err(err) => warn!(peer:% = self.peer_id, error:% = err; "Finished task with error"),
        }
    }

    async fn run_inner(&mut self) -> Result<()> {
        select! {
            peer_handler_request_result_option = self.peer_handler_request_stream.next() => {
                debug!("Received FIDL PeerHandler protocol request {:?} from peer {}",
                   peer_handler_request_result_option, self.peer_id);
               let peer_handler_request_result = peer_handler_request_result_option
                   .ok_or_else(|| format_err!("FIDL Peer protocol request stream closed for peer {}", self.peer_id))?;
               let peer_handler_request = peer_handler_request_result?;
               self.handle_peer_handler_request(peer_handler_request)?;

            }
            at_response_result_option = self.at_connection.next() => {
                // TODO(fxb/127362) Filter unsolicited AT messages.
                debug!("Received AT response {:?} from peer {}",
                    at_response_result_option, self.peer_id);
                let at_response_result =
                    at_response_result_option
                        .ok_or_else(|| format_err!("AT connection stream closed for peer {}", self.peer_id))?;
                let at_response = at_response_result?;

                self.handle_at_response(at_response);
            }
            procedure_outputs_result_option = self.procedure_manager.next() => {
            debug!("Received procedure outputs {:?} for peer {:?}",
                    procedure_outputs_result_option, self.peer_id);

                let procedure_outputs_result =
                    procedure_outputs_result_option
                        .ok_or_else(|| format_err!("Procedure manager stream closed for peer {}", self.peer_id))?;
                let procedure_outputs = procedure_outputs_result?;

                for procedure_output in procedure_outputs {
                    self.handle_procedure_output(procedure_output).await?;
                }
            }
        }
        Ok(())
    }

    fn handle_peer_handler_request(
        &mut self,
        peer_handler_request: fidl_hfp::PeerHandlerRequest,
    ) -> Result<()> {
        // TODO(b/321278917) Refactor this method to be testable.
        // TODO(fxbug.dev/136796) asynchronously respond to requests when a procedure completes.
        let (command_from_hf, responder) = match peer_handler_request {
            fidl_hfp::PeerHandlerRequest::RequestOutgoingCall {
                action: fidl_hfp::CallAction::DialFromNumber(number),
                responder,
            } => (CommandFromHf::CallActionDialFromNumber { number }, responder),
            fidl_hfp::PeerHandlerRequest::RequestOutgoingCall {
                action: fidl_hfp::CallAction::DialFromLocation(memory),
                responder,
            } => (CommandFromHf::CallActionDialFromMemory { memory }, responder),
            fidl_hfp::PeerHandlerRequest::RequestOutgoingCall {
                action: fidl_hfp::CallAction::RedialLast(_),
                responder,
            } => (CommandFromHf::CallActionRedialLast, responder),
            _ => unimplemented!(),
        };

        // TODO(fxbug.dev/136796) asynchronously respond to this request when the procedure
        // completes.
        let send_result = responder.send(Ok(()));
        if let Err(err) = send_result {
            warn!("Error {:?} sending result to peer {:}", err, self.peer_id);
        }

        self.procedure_manager.enqueue(ProcedureInput::CommandFromHf(command_from_hf));
        Ok(())
    }

    fn handle_at_response(&mut self, at_response: at::Response) {
        // TODO(https://fxbug.dev/42077959) Handle unsolicited responses separately.

        let procedure_input = ProcedureInput::AtResponseFromAg(at_response);
        self.procedure_manager.enqueue(procedure_input);
    }

    async fn handle_procedure_output(&mut self, procedure_output: ProcedureOutput) -> Result<()> {
        match procedure_output {
            ProcedureOutput::AtCommandToAg(command) => {
                self.at_connection.write_commands(&[command]).await?
            }
            ProcedureOutput::CommandToHf(CommandToHf::SetAgIndicatorIndex { indicator, index }) => {
                self.ag_indicator_translator.set_index(indicator, index)?
            }
            _ => unimplemented!("Unimplemented ProcedureOutput"),
        };

        Ok(())
    }

    // TODO(fxb/134161) Implement call setup and call transfers.
    #[allow(unused)]
    fn start_audio_connection(&mut self) {
        self.procedure_manager
            .enqueue(ProcedureInput::CommandFromHf(CommandFromHf::StartAudioConnection))
    }
}
