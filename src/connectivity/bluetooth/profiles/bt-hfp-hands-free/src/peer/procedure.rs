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

pub trait ProcedureInputT {
    fn to_initialized_procedure(&self) -> Option<Box<dyn Procedure>>;

    fn can_start_procedure(&self) -> bool;
}

impl ProcedureInputT for ProcedureInput {
    /// Matches a specific input to procedure
    fn to_initialized_procedure(&self) -> Option<Box<dyn Procedure>> {
        match self {
            // TODO(fxb/130999) This is wrong--we need to start SLCI ourselves, not wait for an AT command.
            at_response_pattern!(Brsf) => Some(Box::new(SlcInitProcedure::new())),
            at_response_pattern!(Ciev) => Some(Box::new(PhoneStatusProcedure::new())),
            at_response_pattern!(Bcs) => Some(Box::new(CodecConnectionSetupProcedure::new())),
            _ => None,
        }
    }

    fn can_start_procedure(&self) -> bool {
        match self {
            at_response_pattern!(Brsf) | at_response_pattern!(Ciev) | at_response_pattern!(Bcs) => {
                true
            }
            _ => false,
        }
    }
}

pub trait Procedure: fmt::Debug {
    /// Returns the name of this procedure for logging.
    fn name(&self) -> &str;

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
