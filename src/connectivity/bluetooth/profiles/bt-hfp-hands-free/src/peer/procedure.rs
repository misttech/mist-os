// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Error, Result};
use at_commands as at;
use std::fmt;
use std::fmt::Debug;

use crate::peer::ag_indicators::AgIndicatorIndex;
use crate::peer::procedure_manipulated_state::ProcedureManipulatedState;

#[cfg(test)]
pub mod test;

// Individual procedures
pub mod audio_connection_setup;
pub mod codec_connection_setup;
use codec_connection_setup::CodecConnectionSetupProcedure;

pub mod initiate_call;
use initiate_call::InitiateCallProcedure;

pub mod slc_initialization;
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

// For use below
macro_rules! make_from {
    ($source: path, $destination: ident, $destination_variant: ident) => {
        impl From<$source> for $destination {
            fn from(source: $source) -> $destination {
                $destination::$destination_variant(source)
            }
        }
    };
}

#[derive(Clone, Debug, PartialEq)]
pub enum CommandFromHf {
    CallActionDialFromNumber { number: String },
    CallActionDialFromMemory { memory: String },
    CallActionRedialLast,
    StartAudioConnection,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProcedureInput {
    AtResponseFromAg(at::Response),
    CommandFromHf(CommandFromHf),
}

make_from!(at::Response, ProcedureInput, AtResponseFromAg);
make_from!(CommandFromHf, ProcedureInput, CommandFromHf);

#[derive(Clone, Debug, PartialEq)]
pub enum CommandToHf {
    SetInitialAgIndicatorValues { values: Vec<i64> },
    SetAgIndicatorIndex { indicator: AgIndicatorIndex, index: i64 },
}

#[derive(Clone, Debug, PartialEq)]
pub enum ProcedureOutput {
    AtCommandToAg(at::Command),
    CommandToHf(CommandToHf),
}

make_from!(at::Command, ProcedureOutput, AtCommandToAg);
make_from!(CommandToHf, ProcedureOutput, CommandToHf);

pub trait ProcedureInputT<O: ProcedureOutputT>: Clone + Debug + PartialEq + Unpin {
    fn to_initialized_procedure(&self) -> Option<Box<dyn Procedure<Self, O>>>;

    fn can_start_procedure(&self) -> bool;
}

impl ProcedureInputT<ProcedureOutput> for ProcedureInput {
    /// Matches a specific input to procedure
    fn to_initialized_procedure(&self) -> Option<Box<dyn Procedure<Self, ProcedureOutput>>> {
        match self {
            // TODO(https://fxbug.dev/42081254) This is wrong--we need to start SLCI ourselves, not wait for an AT command.
            at_resp!(Brsf) => Some(Box::new(SlcInitProcedure::new())),

            at_resp!(Bcs) => Some(Box::new(CodecConnectionSetupProcedure::new())),

            ProcedureInput::CommandFromHf(CommandFromHf::CallActionDialFromNumber { .. })
            | ProcedureInput::CommandFromHf(CommandFromHf::CallActionDialFromMemory { .. })
            | ProcedureInput::CommandFromHf(CommandFromHf::CallActionRedialLast) => {
                Some(Box::new(InitiateCallProcedure::new()))
            }

            _ => None,
        }
    }

    fn can_start_procedure(&self) -> bool {
        match self {
            at_resp!(Brsf)
            | at_resp!(Ciev)
            | at_resp!(Bcs)
            | ProcedureInput::CommandFromHf(CommandFromHf::CallActionDialFromNumber { .. })
            | ProcedureInput::CommandFromHf(CommandFromHf::CallActionDialFromMemory { .. })
            | ProcedureInput::CommandFromHf(CommandFromHf::CallActionRedialLast) => true,
            _ => false,
        }
    }
}

pub trait ProcedureOutputT: Clone + Debug + PartialEq + Unpin {}
impl ProcedureOutputT for ProcedureOutput {}

pub trait Procedure<I: ProcedureInputT<O>, O: ProcedureOutputT>: fmt::Debug {
    /// Create a new instance of the procedure.
    fn new() -> Self
    where
        Self: Sized;

    /// Returns the name of this procedure for logging.
    fn name(&self) -> &str;

    /// Receive a ProcedureInput to progress the procedure. Returns an error in updating
    /// the procedure or a ProcedureOutput.
    fn transition(
        &mut self,
        state: &mut ProcedureManipulatedState,
        input: I,
    ) -> Result<Vec<O>, Error>;

    /// Returns true if the Procedure is finished.
    fn is_terminated(&self) -> bool;
}
