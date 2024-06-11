// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![allow(dead_code)]
#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]

use crate::error;
use crate::errors::Errno;

#[derive(Clone, Copy, PartialEq)]
#[repr(usize)]
pub enum ElfNoteType {
    // NT_PRSTATUS
    PrStatus = 1,
    // NT_FPREGSET
    FpRegSet = 2,
    // NT_X86_XSTATE
    X86_XState = 0x202,
}

impl TryFrom<usize> for ElfNoteType {
    type Error = Errno;

    fn try_from(v: usize) -> Result<Self, Errno> {
        match v {
            x if x == ElfNoteType::PrStatus as usize => Ok(ElfNoteType::PrStatus),
            x if x == ElfNoteType::FpRegSet as usize => Ok(ElfNoteType::FpRegSet),
            x if x == ElfNoteType::X86_XState as usize => Ok(ElfNoteType::X86_XState),
            _ => error!(EINVAL),
        }
    }
}
