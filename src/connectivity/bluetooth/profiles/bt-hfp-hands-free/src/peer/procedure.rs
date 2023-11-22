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

macro_rules! at_ok {
    () => {
        ProcedureInput::AtResponseFromAg(at::Response::Ok)
    };
}
pub(crate) use at_ok;

macro_rules! at_resp {
    ($variant: ident) => {
        ProcedureInput::AtResponseFromAg(at::Response::Success(
            at::Success::$variant { .. },
        ))
    };
    ($variant: ident $args: tt) => {
        ProcedureInput::AtResponseFromAg(at::Response::Success(at::Success::$variant $args ))
    };
}
pub(crate) use at_resp;

macro_rules! at_cmd {
    ($variant: ident $args: tt) => {
        ProcedureOutput::AtCommandToAg(at::Command::$variant $args)
    };
}
pub(crate) use at_cmd;

#[derive(Debug, PartialEq)]
pub enum CommandFromHf {}

#[derive(Debug, PartialEq)]
pub enum ProcedureInput {
    AtResponseFromAg(at::Response),
    // TODO(fxb/127025) Use this in task.rs.
    #[allow(unused)]
    CommandFromHf(CommandFromHf),
}

#[derive(Debug, PartialEq)]
pub enum CommandToHf {}

#[derive(Debug, PartialEq)]
pub enum ProcedureOutput {
    AtCommandToAg(at::Command),
    // TODO(fxbug.dev/127025) use this in PeerTask and procedures.
    #[allow(unused)]
    CommandToHf(CommandToHf),
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
            at_resp!(Brsf) => Some(Box::new(SlcInitProcedure::new())),
            at_resp!(Ciev) => Some(Box::new(PhoneStatusProcedure::new())),
            at_resp!(Bcs) => Some(Box::new(CodecConnectionSetupProcedure::new())),
            _ => None,
        }
    }

    fn can_start_procedure(&self) -> bool {
        match self {
            at_resp!(Brsf) | at_resp!(Ciev) | at_resp!(Bcs) => true,
            _ => false,
        }
    }
}

pub trait Procedure: fmt::Debug {
    /// Returns the name of this procedure for logging.
    fn name(&self) -> &str;

    /// Receive a ProcedureInput to progress the procedure. Returns an error in updating
    /// the procedure or a ProcedureOutput.
    fn transition(
        &mut self,
        state: &mut ProcedureManipulatedState,
        input: ProcedureInput,
    ) -> Result<Vec<ProcedureOutput>, Error>;

    /// Returns true if the Procedure is finished.
    fn is_terminated(&self) -> bool;
}
