// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[derive(thiserror::Error, Debug)]
pub enum EbpfError {
    #[error("Unable to create VM")]
    VmInitialization,

    #[error("VM error registering callback: {0}")]
    VmRegisterError(String),

    #[error("Verification error loading program: {0}")]
    ProgramLoadError(String),

    #[error("VM error loading program: {0}")]
    VmLoadError(String),

    #[error("Unknown CBPF {element_type} {value} for {op}")]
    UnrecognizedCbpfError { element_type: String, value: String, op: String },

    #[error("Scratch buffer overrun: Starnix only supports 3 scratch memory locations")]
    ScratchBufferOverflow,
}
