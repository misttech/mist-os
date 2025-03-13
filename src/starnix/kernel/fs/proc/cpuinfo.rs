// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::LazyLock;

use fuchsia_component::client::connect_to_protocol_sync;

use crate::vfs::{DynamicFile, DynamicFileBuf, DynamicFileSource, FsNodeOps};
use starnix_logging::log_error;
use starnix_uapi::errors::Errno;

#[derive(Clone)]
pub struct CpuinfoFile {}
impl CpuinfoFile {
    pub fn new_node() -> impl FsNodeOps {
        DynamicFile::new_node(Self {})
    }
}

impl DynamicFileSource for CpuinfoFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let is_qemu = SYSINFO.is_qemu();

        for i in 0..zx::system_get_num_cpus() {
            writeln!(sink, "processor\t: {}", i)?;

            // Report emulated CPU as "QEMU Virtual CPU". Some LTP tests rely on this to detect
            // that they running in a VM.
            if is_qemu {
                writeln!(sink, "model name\t: QEMU Virtual CPU")?;
            }

            writeln!(sink)?;
        }
        Ok(())
    }
}

struct SysInfo {
    board_name: String,
}

impl SysInfo {
    fn is_qemu(&self) -> bool {
        matches!(
            self.board_name.as_str(),
            "Standard PC (Q35 + ICH9, 2009)" | "qemu-arm64" | "qemu-riscv64"
        )
    }

    fn fetch() -> Result<SysInfo, anyhow::Error> {
        let sysinfo = connect_to_protocol_sync::<fidl_fuchsia_sysinfo::SysInfoMarker>()?;
        let board_name = match sysinfo.get_board_name(zx::MonotonicInstant::INFINITE)? {
            (zx::sys::ZX_OK, Some(name)) => name,
            (_, _) => "Unknown".to_string(),
        };
        Ok(SysInfo { board_name })
    }
}

const SYSINFO: LazyLock<SysInfo> = LazyLock::new(|| {
    SysInfo::fetch().unwrap_or_else(|e| {
        log_error!("Failed to fetch sysinfo: {e}");
        SysInfo { board_name: "Unknown".to_string() }
    })
});
