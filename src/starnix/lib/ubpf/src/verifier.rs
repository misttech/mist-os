// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use linux_uapi::bpf_insn;

use crate::UbpfError;

pub fn verify(_code: &Vec<bpf_insn>) -> Result<(), UbpfError> {
    Ok(())
}
