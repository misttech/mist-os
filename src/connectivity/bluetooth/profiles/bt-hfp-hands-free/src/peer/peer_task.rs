// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Result};
use bt_hfp::call::Direction;
use bt_hfp::codec_id::CodecId;
use bt_hfp::{a2dp, audio, sco};
use fuchsia_bluetooth::types::{Channel, PeerId};
use fuchsia_sync::Mutex;
use futures::{select, StreamExt};
use log::{debug, error, info, warn};
use std::sync::Arc;
use vigil::{DropWatch, Vigil};
use {at_commands as at, fidl_fuchsia_bluetooth_hfp as fidl_hfp, fuchsia_async as fasync};

use crate::config::HandsFreeFeatureSupport;
use crate::peer::ag_indicators::{AgIndicator, AgIndicatorTranslator};
use crate::peer::at_connection::{self, Response as AtResponse};
use crate::peer::calls::Calls;
use crate::peer::procedure::{CommandFromHf, CommandToHf, ProcedureInput, ProcedureOutput};
use crate::peer::procedure_manager::ProcedureManager;

const DEFAULT_CODEC: CodecId = CodecId::CVSD;

pub struct PeerTask {
    peer_id: PeerId,
    procedure_manager: ProcedureManager<ProcedureInput, ProcedureOutput>,
    peer_handler_request_stream: fidl_hfp::PeerHandlerRequestStream,
    at_connection: at_connection::Connection,
    ag_indicator_translator: AgIndicatorTranslator,
    calls: Calls,
    sco_connector: sco::Connector,
    sco_state: sco::InspectableState,
    a2dp_control: a2dp::Control,
    audio_control: Arc<Mutex<Box<dyn audio::Control>>>,
}

impl PeerTask {
    pub fn spawn(
        peer_id: PeerId,
        config: HandsFreeFeatureSupport,
        peer_handler_request_stream: fidl_hfp::PeerHandlerRequestStream,
        rfcomm: Channel,
        sco_connector: sco::Connector,
        audio_control: Arc<Mutex<Box<dyn audio::Control>>>,
    ) -> fasync::Task<()> {
        let procedure_manager = ProcedureManager::new(peer_id, config);
        let at_connection = at_connection::Connection::new(peer_id, rfcomm);
        let calls = Calls::new(peer_id);
        let ag_indicator_translator = AgIndicatorTranslator::new();
        let sco_state = sco::InspectableState::default();
        let a2dp_control = a2dp::Control::connect();

        let mut peer_task = Self {
            peer_id,
            procedure_manager,
            peer_handler_request_stream,
            at_connection,
            ag_indicator_translator,
            calls,
            sco_connector,
            sco_state,
            a2dp_control,
            audio_control,
        };

        // Set SCO state to AwaitingRemote
        peer_task.await_remote_sco();

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
        // Since IOwned doesn't implement DerefMut, we need to get a mutable reference to the
        // underlying SCO state value to call mutable methods on it.  As this invalidates
        // the enclosing struct, we need to drop this reference explicitiy in every match arm so
        // we can can call method on self.
        let mut sco_state = self.sco_state.as_mut();
        let mut on_sco_closed = sco_state.on_closed();

        select! {
            peer_handler_request_result_option = self.peer_handler_request_stream.next() => {
                debug!("Received FIDL PeerHandler protocol request {:?} from peer {}",
                   peer_handler_request_result_option, self.peer_id);

                drop(sco_state);

                let peer_handler_request_result = peer_handler_request_result_option
                    .ok_or_else(||format_err!("FIDL Peer protocol request stream closed for peer {}", self.peer_id))?;
                let peer_handler_request = peer_handler_request_result?;

                self.handle_peer_handler_request(peer_handler_request)?;
            }
            at_response_result_option = self.at_connection.next() => {
                // TODO(https://fxbug.dev/127362) Filter unsolicited AT messages.
                debug!("Received AT response {:?} from peer {}",
                    at_response_result_option, self.peer_id);

                drop(sco_state);

                let at_response_result =
                    at_response_result_option
                        .ok_or_else(|| format_err!("AT connection stream closed for peer {}", self.peer_id))?;
                let at_response = at_response_result?;

                self.handle_at_response(at_response);
            }
            procedure_outputs_result_option = self.procedure_manager.next() => {
                debug!("Received procedure outputs {:?} for peer {:?}",
                    procedure_outputs_result_option, self.peer_id);

                drop(sco_state);

                let procedure_outputs_result =
                    procedure_outputs_result_option
                        .ok_or_else(|| format_err!("Procedure manager stream closed for peer {}", self.peer_id))?;
                let procedure_outputs = procedure_outputs_result?;

                for procedure_output in procedure_outputs {
                    self.handle_procedure_output(procedure_output).await?;
                }
            }
            call_procedure_input_result_option = self.calls.next() => {
                info!("Received call procedure input {:?} for peer {:}", call_procedure_input_result_option, self.peer_id);

                drop(sco_state);

                let call_procedure_input_result =
                    call_procedure_input_result_option
                        .ok_or_else(|| format_err!("Calls stream closed for peer {:}", self.peer_id))?;
                let call_procedure_input = call_procedure_input_result?;

                self.procedure_manager.enqueue(call_procedure_input)
            }
            sco_connection_result = sco_state.on_connected() => {
                info!("Received SCO connection for peer {:}", self.peer_id);
                drop(sco_state);
                match sco_connection_result {
                    Ok(sco_connection) if !sco_connection.is_closed() => self.handle_sco_connection(sco_connection).await?,
                    // This can occur if the AG opens and closes a SCO connection immediately.
                    Ok(_closed_sco_connection) => info!("Got closed SCO connection from peer {}", self.peer_id),
                    Err(err) => {
                        warn!("Got error receiving SCO connection for peer {}.", self.peer_id);
                        Err(err)?;
                    }
                }
            }
            _ = on_sco_closed => {
                info!("SCO connection closed for peer {:}", self.peer_id);
                drop(sco_state);
                self.await_remote_sco();
            }
        }

        Ok(())
    }

    fn handle_peer_handler_request(
        &mut self,
        peer_handler_request: fidl_hfp::PeerHandlerRequest,
    ) -> Result<()> {
        // TODO(b/321278917) Refactor this method to be testable. Maybe move it to calls.rs
        // TODO(fxbug.dev/136796) asynchronously respond to requests when a procedure completes.
        let (command_from_hf, responder) = match peer_handler_request {
            fidl_hfp::PeerHandlerRequest::RequestOutgoingCall {
                action: fidl_hfp::CallAction::DialFromNumber(number),
                responder,
            } => {
                let _index = self.calls.insert_new_call(
                    /* state = */ None,
                    Some(number.clone().into()),
                    Direction::MobileOriginated,
                );
                (CommandFromHf::CallActionDialFromNumber { number }, responder)
            }
            fidl_hfp::PeerHandlerRequest::RequestOutgoingCall {
                action: fidl_hfp::CallAction::DialFromLocation(memory),
                responder,
            } => {
                let _index = self.calls.insert_new_call(
                    /* state = */ None,
                    /* number = */ None,
                    Direction::MobileOriginated,
                );
                (CommandFromHf::CallActionDialFromMemory { memory }, responder)
            }
            fidl_hfp::PeerHandlerRequest::RequestOutgoingCall {
                action: fidl_hfp::CallAction::RedialLast(_),
                responder,
            } => {
                let _index = self.calls.insert_new_call(
                    /* state = */ None,
                    /* number = */ None,
                    Direction::MobileOriginated,
                );
                (CommandFromHf::CallActionRedialLast, responder)
            }
            _ => {
                return Err(format_err!(
                    "Unimplemented peer handle request {peer_handler_request:?}"
                ))
            }
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

    fn handle_at_response(&mut self, at_response: AtResponse) {
        if Self::at_response_is_unsolicted(&at_response) {
            self.handle_unsolicited_response(at_response)
        } else {
            let procedure_input = ProcedureInput::AtResponseFromAg(at_response);
            self.procedure_manager.enqueue(procedure_input);
        }
    }

    fn at_response_is_unsolicted(at_response: &AtResponse) -> bool {
        match at_response {
            AtResponse::Recognized(at::Response::Success(at::Success::Ciev { .. }))
            | AtResponse::Recognized(at::Response::Success(at::Success::Clip { .. })) => true,
            _ => false,
        }
    }

    // Must take an unolicited AT Response--a +CIEV or a +CLIP.
    fn handle_unsolicited_response(&mut self, at_response: AtResponse) {
        match at_response {
            AtResponse::Recognized(at::Response::Success(at::Success::Ciev { ind, value })) => {
                let indicator = self.ag_indicator_translator.translate_indicator(ind, value);
                match indicator {
                    Ok(ind) => self.handle_ag_indicator(ind),
                    // HFP v1.8 sec. 4.10: Unknown indicators are not an error.
                    Err(err) => info!("Got unknown indicator: {:?}", err),
                }
            }
            // TODO(htps://fxbug.dec/135158) Handle setting phone numbers.
            AtResponse::Recognized(at::Response::Success(at::Success::Clip { .. })) => {
                warn!("+CLIP not handled")
            }
            _ => warn!("Received unexpected unsolicited AT response: {:?}", at_response),
        }
    }

    fn handle_ag_indicator(&mut self, indicator: AgIndicator) -> () {
        match indicator {
            AgIndicator::Call(call_indicator) => {
                self.calls.set_call_state_by_indicator(call_indicator)
            }
            // TODO(https://fxbug.dev/131814) Handle NetworkInformation indicators.
            // TODO(https://fxbug.dev/131815) Handle BatteryCharge indicators.
            _ => {
                error!("Handling indicator {:?} unimplemented.", indicator);
            }
        }
    }

    async fn handle_procedure_output(&mut self, procedure_output: ProcedureOutput) -> Result<()> {
        match procedure_output {
            ProcedureOutput::AtCommandToAg(command) => {
                self.at_connection.write_commands(&[command]).await?
            }
            ProcedureOutput::CommandToHf(CommandToHf::SetAgIndicatorIndex { indicator, index }) => {
                self.ag_indicator_translator.set_index(indicator, index)?
            }
            ProcedureOutput::CommandToHf(CommandToHf::AwaitRemoteSco) => {
                // We've renegotiated the codec, so wait for a SCO connection with the new codec.
                self.await_remote_sco()
            }
            // TODO(https://fxbug.dev/131814) Set initial NetworkInformation indicator value from
            // SetInitialIndicatorValues procedure output.
            // TODO(https://fxbug.dev/131815) Set initial BatteryCharge indicator value from
            // SetInitialIndicatorValues procedure output.
            _ => return Err(format_err!("Unimplemented ProcedureOutput")),
        };

        Ok(())
    }

    async fn handle_sco_connection(&mut self, sco_connection: sco::Connection) -> Result<()> {
        let pause_token = self.pause_a2dp_audio().await?;
        let vigil = self.watch_active_sco(&sco_connection, pause_token);
        self.start_hfp_audio(sco_connection)?;

        self.sco_state.iset(sco::State::Active(vigil));

        Ok(())
    }

    async fn pause_a2dp_audio(&mut self) -> Result<a2dp::PauseToken> {
        let res = self.a2dp_control.pause(Some(self.peer_id.clone())).await;
        let pause_token = match res {
            Err(err) => {
                Err(format_err!("Couldn't pause A2DP Audio for peer {}: {:?}", self.peer_id, err))
            }
            Ok(token) => {
                info!("Successfully paused A2DP audio for peer {}", self.peer_id);
                Ok(token)
            }
        };

        pause_token
    }

    fn start_hfp_audio(&self, sco_connection: sco::Connection) -> Result<()> {
        let mut audio = self.audio_control.lock();
        let codec_id = CodecId::from_parameter_set(&sco_connection.params.parameter_set);
        if let Err(err) = audio.start(self.peer_id.clone(), sco_connection, codec_id) {
            Err(format_err!("Couldn't start HFP audio for peer {}: {:?}", self.peer_id, err))?;
        } else {
            info!("Successfully started HFP audio for peer {}", self.peer_id);
        }

        Ok(())
    }

    fn watch_active_sco(
        &self,
        sco_connection: &sco::Connection,
        pause_token: a2dp::PauseToken,
    ) -> Vigil<sco::Active> {
        let vigil = Vigil::new(sco::Active::new(sco_connection, pause_token));
        let peer_id = self.peer_id.clone();
        let audio_control = self.audio_control.clone();

        Vigil::watch(&vigil, {
            move |_| match audio_control.lock().stop(peer_id) {
                Err(err) => warn!("Couldn't stop HFP audio for peer {}: {:?}", peer_id, err),
                Ok(()) => info!("Succesfully stopped HFP audio, for peer {}", peer_id),
            }
        });

        vigil
    }

    // TODO(https://fxbug.dev/134161) Implement call setup and call transfers.
    #[allow(unused)]
    fn start_audio_connection(&mut self) {
        self.procedure_manager
            .enqueue(ProcedureInput::CommandFromHf(CommandFromHf::StartAudioConnection))
    }

    fn get_selected_codec(&self) -> CodecId {
        let codec_option = self.procedure_manager.procedure_manipulated_state.selected_codec;
        codec_option.unwrap_or(DEFAULT_CODEC)
    }

    fn await_remote_sco(&mut self) {
        let codecs = vec![self.get_selected_codec()];
        let fut = self.sco_connector.accept(self.peer_id.clone(), codecs);
        self.sco_state.iset(sco::State::AwaitingRemote(Box::pin(fut)));
    }
}
