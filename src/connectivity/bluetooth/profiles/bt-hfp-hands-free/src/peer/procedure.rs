// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use at_commands as at;
use std::fmt;

pub mod codec_connection_setup;
pub mod phone_status;
pub mod slc_initialization;

use crate::peer::procedure_manipulated_state::ProcedureManipulatedState;
use codec_connection_setup::CodecConnectionSetupProcedure;
use phone_status::PhoneStatusProcedure;
use slc_initialization::SlcInitProcedure;

macro_rules! at_response_pattern {
    ($response_variant: ident) => {
        ProcedureInput::AtResponseFromAg(at::Response::Success(
            at::Success::$response_variant { .. },
        ))
    };
}
pub(crate) use at_response_pattern;

// TODO(fxr/127025) use this in PeerTask and procedures.
#[allow(unused)]
#[derive(Debug)]
pub enum CommandFromHf {}

#[derive(Debug)]
pub enum ProcedureInput {
    AtResponseFromAg(at::Response),
    // TODO(fxb/127025) Use this in task.rs.
    #[allow(unused)]
    CommandFromHf(CommandFromHf),
}

// TODO(fxr/127025) use this in PeerTask and procedures.
#[allow(unused)]
#[derive(Debug)]
pub enum CommandToHf {}

#[derive(Debug)]
pub enum ProcedureOutput {
    AtCommandsToAg(Vec<at::Command>),
    // TODO(fxr/127025) use this in PeerTask and procedures.
    #[allow(unused)]
    CommandToHf(CommandToHf),
    #[allow(unused)]
    None,
}

// TODO(fxb/127356) Get rid of ProcedureMarker and move this behavior to ProcedureInput
#[derive(Debug, Eq, Hash, PartialEq, Clone, Copy)]
pub enum ProcedureMarker {
    /// The Service Level Connection Initialization procedure as defined in HFP v1.8 Section 4.2.
    SlcInitialization,
    /// The Transfer of Phone Status procedures as defined in HFP v1.8 Section 4.4 - 4.7.
    PhoneStatus,
    /// The Codec Connection Setup where the AG informs the HF which codeic ID will be used.
    CodecConnectionSetup,
}

impl ProcedureMarker {
    /// Matches a specific marker to procedure
    pub fn initialize(&self) -> Box<dyn Procedure> {
        match self {
            Self::SlcInitialization => Box::new(SlcInitProcedure::new()),
            Self::PhoneStatus => Box::new(PhoneStatusProcedure::new()),
            Self::CodecConnectionSetup => Box::new(CodecConnectionSetupProcedure::new()),
        }
    }

    /// Returns the ProcedureMarker for the procedure that this ProcedureInput can start.
    pub fn procedure_from_initial_input(
        slc_initialized: bool,
        input: &ProcedureInput,
    ) -> Option<Self> {
        match input {
            at_response_pattern!(Brsf) if !slc_initialized => Some(Self::SlcInitialization),
            at_response_pattern!(Ciev) => Some(Self::PhoneStatus),
            at_response_pattern!(Bcs) => Some(Self::CodecConnectionSetup),
            _ => None,
        }
    }
}

pub trait Procedure: fmt::Debug {
    /// Returns the unique identifier associated with this procedure.
    fn marker(&self) -> ProcedureMarker;

    /// Receive an AG `update` to progress the procedure. Returns an error in updating
    /// the procedure or command(s) to be sent back to AG
    ///
    /// Developers should ensure that the final request of a Procedure does not require
    /// a response.
    fn transition(
        &mut self,
        state: &mut ProcedureManipulatedState,
        input: &Vec<at::Response>,
    ) -> Result<Vec<at::Command>, Error>;

    /// Returns true if the Procedure is finished.
    fn is_terminated(&self) -> bool;
}
