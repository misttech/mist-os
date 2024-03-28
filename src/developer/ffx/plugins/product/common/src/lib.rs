// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use fho::{Result, VerifiedMachineWriter};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::rc::Rc;
use structured_ui::{Interface, Presentation, Response};

/// CommandStatus is returned to indicate exit status of
/// a command. The Ok variant is optional, and is intended
/// for use with commands that return no other data so there
/// is some indication of correct execution.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    /// Successful execution with an optional informational string.
    Ok { message: Option<String> },
    /// Unexpected error with string.
    UnexpectedError { message: String },
    /// A known kind of error that can be reported usefully to the user
    UserError { message: String },
}

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "type")]
pub enum MachineOutput<T: JsonSchema + Serialize> {
    CommandStatus(CommandStatus),
    Notice { title: Option<String>, message: Option<String> },
    Data(T),
}

// A structured_ui::Interface implementation for use
// when machine output is needed.
pub struct MachineUi<T: Serialize + JsonSchema> {
    writer: Rc<RefCell<VerifiedMachineWriter<MachineOutput<T>>>>,
}

impl<T: Serialize + JsonSchema> Interface for MachineUi<T> {
    fn present(&self, output: &structured_ui::Presentation) -> anyhow::Result<Response> {
        match output {
            Presentation::Notice(notice) => self.machine(MachineOutput::<T>::Notice {
                title: notice.get_title(),
                message: notice.get_message(),
            })?,
            Presentation::Progress(_) => (), //ignore progress for machine output.
            Presentation::StringPrompt(p) => {
                todo!("String prompt not supported in machine mode: {p:?}")
            }
            Presentation::Table(_table) => todo!("Table not supported in machine mode: {_table:?}"),
        };

        Ok(Response::Default)
    }
}

impl<T: Serialize + JsonSchema> MachineUi<T> {
    pub fn new(writer: VerifiedMachineWriter<MachineOutput<T>>) -> Self {
        MachineUi { writer: Rc::new(RefCell::new(writer)) }
    }

    pub fn machine(&self, data: MachineOutput<T>) -> Result<()> {
        self.writer.borrow_mut().machine(&data).map_err(move |e| e.into())
    }
}
