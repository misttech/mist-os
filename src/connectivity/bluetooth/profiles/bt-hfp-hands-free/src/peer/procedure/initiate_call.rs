// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{format_err, Result};
use at_commands as at;

use super::{at_cmd, at_ok, CommandFromHf, Procedure, ProcedureInput, ProcedureOutput};

use crate::peer::procedure_manipulated_state::ProcedureManipulatedState;

#[derive(Debug, PartialEq)]
pub enum InitiateCallProcedure {
    Started,
    WaitingForOk,
    Terminated,
}

/// HFP v1.8 §§ 4.18, 4.19, 4.20
///
/// This procedure only handles sending the AT Commands to start call setup.  The rest of these
/// procedures come in as unsolicited +CIEVs and SCO setup, which are handled separately.
impl Procedure<ProcedureInput, ProcedureOutput> for InitiateCallProcedure {
    fn new() -> Self {
        Self::Started
    }

    fn name(&self) -> &str {
        "Initiate Call Procedure"
    }

    fn transition(
        &mut self,
        _state: &mut ProcedureManipulatedState,
        input: ProcedureInput,
    ) -> Result<Vec<ProcedureOutput>> {
        let output;
        match (&self, input) {
            (
                Self::Started,
                ProcedureInput::CommandFromHf(CommandFromHf::CallActionDialFromNumber { number }),
            ) => {
                *self = Self::WaitingForOk;
                output = vec![at_cmd!(AtdNumber { number })];
            }
            (
                Self::Started,
                ProcedureInput::CommandFromHf(CommandFromHf::CallActionDialFromMemory { memory }),
            ) => {
                *self = Self::WaitingForOk;
                output = vec![at_cmd!(AtdMemory { location: memory })];
            }
            (Self::Started, ProcedureInput::CommandFromHf(CommandFromHf::CallActionRedialLast)) => {
                *self = Self::WaitingForOk;
                output = vec![at_cmd!(Bldn {})];
            }

            (Self::WaitingForOk, at_ok!()) => {
                *self = Self::Terminated;
                output = vec![]
            }

            (_, input) => {
                return Err(format_err!(
                    "Received invalid response {:?} during an initiate call procedure in state {:?}.",
                    input, self
                ));
            }
        }

        Ok(output)
    }

    fn is_terminated(&self) -> bool {
        *self == Self::Terminated
    }
}
