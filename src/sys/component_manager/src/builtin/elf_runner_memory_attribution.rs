// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module implements the `fuchsia.memory.report/SnapshotProvider` FIDL protocol
//! capability, which is only provided by the ELF runner.
//!
//! TODO(https://fxbug.dev/305862055): Once the ELF runner becomes a component that can
//! expose its own protocols, we don't need to offer this as a built-in.

use elf_runner::ElfRunner;
use fidl_fuchsia_memory_report as freport;
use std::sync::Arc;

pub struct ElfRunnerMemoryAttribution(Arc<ElfRunner>);

impl ElfRunnerMemoryAttribution {
    pub fn new(elf_runner: Arc<ElfRunner>) -> Self {
        ElfRunnerMemoryAttribution(elf_runner)
    }

    pub fn serve(&self, stream: freport::SnapshotProviderRequestStream) {
        self.0.serve_memory_reporter(stream);
    }
}
