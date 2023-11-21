// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Result};
use at_commands as at;
use fuchsia_bluetooth::types::PeerId;
use futures::stream::FusedStream;
use futures::Stream;
use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};
use tracing::warn;

use super::procedure::{Procedure, ProcedureInput, ProcedureInputT, ProcedureOutput};
use super::procedure_manipulated_state::ProcedureManipulatedState;

use crate::config::HandsFreeFeatureSupport;

// Manage the execution of procedures.
//
// `ProcedureInputs` are enqueued without blocking by clients of this module,
// and the struct implements `Stream`.  `poll_next`ing the stream will drive
// the procedure management state machine.
//
// `ProcedureInput`s are of two kinds: those that are the first input to a
// procedure, and all others.  These are disjoint--for example, an unsolicited
// +BCS AT response received from a peer will always start a Codec Connection
// Setup procedure, but an OK received from a peer will never start a
// procedure.
//
// The method to enqueue `ProcedureInputs`, `enqueue_procedure_input` will
// check to see which type of `ProcedureInput` it is called with, and will
// enqueue it either for the current procedure if it cannot start a new
// procedure, or in a different queue to start a future procedure if it can
// start a new procedure.
//
// The procedure manager contains an optional current procedure. When the
// procedure manager is driven by its `poll_next` method, it checks to see if
// there is a current procedure and an enqueued input for that procedure.  If
// so, it runs the procedure one step with this input and returns the procedure
// output.  If there is no current procedure, if there is a `ProcedureInput`
// enqueued which can start a procedure, it is used to do so, and the output
// from the prodedure is returned.

pub struct ProcedureManager {
    /// ID of the peer for logging.
    peer_id: PeerId,
    /// Current procedure for responses to be routed to.
    current_procedure: Option<Box<dyn Procedure>>,
    /// Commands that will be processed by the current procedure.
    current_procedure_inputs: VecDeque<ProcedureInput>,
    /// Commands that will start new procedures that will be processed when the current procedure
    /// has finished.
    future_procedure_inputs: VecDeque<ProcedureInput>,
    /// Waker for the stream of outputs from procedure state machines managed by this struct.
    stream_waker: Option<Waker>,
    /// Collection of shared features and indicators between two
    /// devices that procedures can change.
    pub procedure_managed_state: ProcedureManipulatedState,
}

impl ProcedureManager {
    pub fn new(peer_id: PeerId, config: HandsFreeFeatureSupport) -> Self {
        let procedure_managed_state = ProcedureManipulatedState::new(config);
        Self {
            peer_id,
            current_procedure: None,
            current_procedure_inputs: VecDeque::new(),
            future_procedure_inputs: VecDeque::new(),
            stream_waker: None,
            procedure_managed_state,
        }
    }

    // Inserts a ProcedureInput into the proper queue. If it can start a
    // procedure, it goes into the future_procedure_inputs queue; otherwise it
    // goes into the current_procedure_inputs queue.
    pub fn enqueue(&mut self, input: ProcedureInput) {
        if let Some(waker) = self.stream_waker.take() {
            waker.wake();
        }

        if input.can_start_procedure() {
            self.future_procedure_inputs.push_back(input);
        } else {
            self.current_procedure_inputs.push_back(input);
        }
    }

    // TODO(fxb/127381) Convert procedures to use ProcedureInputs and remove this method.
    fn extract_response_from_input(input: ProcedureInput) -> Vec<at::Response> {
        match input {
            ProcedureInput::AtResponseFromAg(response) => vec![response],
            _ => unimplemented!(),
        }
    }

    // Must be called with current_procedure set to Some(...)
    fn get_current_procedure_input(&mut self) -> Option<Result<ProcedureInput>> {
        assert!(self.current_procedure.is_some());

        let Some(input) = self.current_procedure_inputs.pop_front() else {
            return None;
        };

        Some(Ok(input))
    }

    // Must be called with current_procedure set to None.
    fn set_new_procedure_and_get_input(&mut self) -> Option<Result<ProcedureInput>> {
        assert!(self.current_procedure.is_none());

        let Some(input) = self.future_procedure_inputs.pop_front() else {
            return None;
        };

        let procedure_option = input.to_initialized_procedure();
        match procedure_option {
            Some(proc) => self.current_procedure = Some(proc),
            None => {
                return Some(Err(format_err!(
                    "Unable to match procedure for input {:?} from peer {:}",
                    input,
                    self.peer_id
                )))
            }
        };

        Some(Ok(input))
    }

    fn run_procedure_with_input(
        &mut self,
        input: ProcedureInput,
    ) -> Option<Result<ProcedureOutput>> {
        // The current procedure was already set or is was set by
        // `set_new_procedure_and_get_input` if it returned successfully.
        let procedure = self.current_procedure.as_mut().unwrap();
        let result = procedure.transition(
            &mut self.procedure_managed_state,
            &Self::extract_response_from_input(input),
        );
        // TODO(fxb/127381) Convert procedures to use ProcedureOutputs and remove this.
        let result = result.map(ProcedureOutput::AtCommandsToAg);
        Some(result)
    }

    /// Run the current procedure if it exists an there are inputs for it, without checking for
    /// errors cases or cleaning up terminated procedures.
    fn run_current_or_new_procedure(&mut self) -> Option<Result<ProcedureOutput>> {
        let input = if self.current_procedure.is_some() {
            self.get_current_procedure_input()
        } else {
            self.set_new_procedure_and_get_input()
        };

        match input {
            Some(Ok(input)) => self.run_procedure_with_input(input),
            // Repackage unsuccessful variants.
            Some(Err(err)) => Some(Err(err)),
            None => None,
        }
    }

    // Confirm there are no inputs for the current procedure when no procedure exists or can be
    // started
    fn check_no_procedure_inputs_for_no_procedure(&self) {
        let current_procedure = self.current_procedure.is_some();
        let startable_procedure = !self.future_procedure_inputs.is_empty();
        let procedure_inputs = !self.current_procedure_inputs.is_empty();

        if !current_procedure && !startable_procedure && procedure_inputs {
            warn!(
                "Inputs ({:?}) queued for non-existent procedure for peer {}",
                self.current_procedure_inputs, self.peer_id
            );
        }
    }

    // Confirm there are no inputs for the current procedure when the existing proceudure is
    // terminated.
    fn check_no_unused_procedure_inputs_for_terminated_procedure(&self) {
        let terminated_procedure = match self.current_procedure.as_ref() {
            Some(procedure) => procedure.is_terminated(),
            None => false,
        };

        let procedure_inputs = !self.current_procedure_inputs.is_empty();

        if terminated_procedure && procedure_inputs {
            warn!(
                "Inputs ({:?}) queued for terminated procedure {:?} for peer {}",
                self.current_procedure_inputs,
                self.current_procedure.as_ref().map(|p| p.name()).unwrap_or("None"),
                self.peer_id
            );
        }
    }

    /// Run the current procedure if one exists and there are inputs for it.  Otherwise starts the
    /// next procedure if there is an input for it.
    ///
    /// Returns None if there is no procedure to run or input for it, or Some of a Result output by
    /// the procedure.
    fn run_next_procedure(&mut self) -> Option<Result<ProcedureOutput>> {
        // Confirm there are no inputs for the current procedure when no procedure exists.  This
        // would mean we've received inputs since the last procedure was terminated or before any
        // procedure was started. This indicates either a bug in the previous procedure or a
        // nonconformant peer.
        self.check_no_procedure_inputs_for_no_procedure();

        let output = self.run_current_or_new_procedure();

        // Confirm there are no inputs for the current procedure when it is terminated.  This
        // would mean we terminated the procedure before consuming all its inputs.  This indicates
        // either a bug in the procedure or a nonconformant peer.
        self.check_no_unused_procedure_inputs_for_terminated_procedure();

        // Clean up terminated procedures.
        if let Some(procedure) = self.current_procedure.as_mut() {
            if procedure.is_terminated() {
                self.current_procedure = None;
            }
        }

        // Clean up unused inputs. Any errors from this should already have been logged.
        if self.current_procedure.is_none() {
            self.current_procedure_inputs.clear()
        }

        output
    }
}

impl Stream for ProcedureManager {
    type Item = Result<ProcedureOutput>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.run_next_procedure() {
            Some(result) => Poll::Ready(Some(result)),
            None => {
                self.stream_waker = Some(context.waker().clone());
                Poll::Pending
            }
        }
    }
}

impl FusedStream for ProcedureManager {
    fn is_terminated(&self) -> bool {
        // It is never the case that the ProcedureManager is terminated--a client could always
        // enqueue new procedure inputs.
        false
    }
}
