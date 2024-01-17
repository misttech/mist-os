// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! See https://www.kernel.org/doc/html/latest/admin-guide/sysrq.html.

use crate::{
    task::{CurrentTask, Kernel},
    vfs::{
        FileObject, FileOps, FsNode, FsNodeHandle, FsNodeOps, FsStr, InputBuffer, OutputBuffer,
        SeekTarget,
    },
};
use fidl_fuchsia_hardware_power_statecontrol::{AdminMarker, RebootReason};
use fuchsia_component::client::connect_to_protocol_sync;
use fuchsia_zircon as zx;
use starnix_logging::{log_warn, not_implemented};
use starnix_uapi::{
    auth::FsCred, device_type::DeviceType, error, errors::Errno, file_mode::FileMode, off_t,
    open_flags::OpenFlags,
};

pub struct SysRqNode {}

impl SysRqNode {
    pub fn new(_kernel: &Kernel) -> Self {
        Self {}
    }
}

impl FsNodeOps for SysRqNode {
    fn create_file_ops(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(SysRqFile {}))
    }

    fn mknod(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _mode: FileMode,
        _dev: DeviceType,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        error!(EINVAL)
    }

    fn mkdir(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _mode: FileMode,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        error!(EINVAL)
    }

    fn create_symlink(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _target: &FsStr,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        error!(EINVAL)
    }

    fn unlink(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        error!(EINVAL)
    }

    fn truncate(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _length: u64,
    ) -> Result<(), Errno> {
        // This file doesn't store any contents, but userspace expects to truncate it on open.
        Ok(())
    }
}

pub struct SysRqFile {}

impl FileOps for SysRqFile {
    fn is_seekable(&self) -> bool {
        false
    }

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        error!(EINVAL)
    }

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let commands = data.read_all()?;
        for command in &commands {
            match *command {
                b'b' => not_implemented!(fxb@319745106, "SysRqRebootNoSync"),
                b'c' => {
                    log_warn!("SysRq kernel crash request.");

                    // Attempt to "crash" the whole device, if that fails settle for just Starnix.
                    // When this call succeeds with a production implementation it should never
                    // return. If it returns at all it is a sign the kernel either doesn't have the
                    // capability or there was a problem with the shutdown request.
                    let reboot_res = connect_to_protocol_sync::<AdminMarker>()
                        .unwrap()
                        .reboot(RebootReason::CriticalComponentFailure, zx::Time::INFINITE);

                    // LINT.IfChange
                    panic!(
                        "reboot call returned unexpectedly ({:?}), crashing from SysRq",
                        reboot_res
                    );
                    // LINT.ThenChange(src/starnix/tests/sysrq/src/lib.rs)
                }
                b'd' => not_implemented!(fxb@319745106, "SysRqDumpLocksHeld"),
                b'e' => not_implemented!(fxb@319745106, "SysRqSigtermAllButInit"),
                b'f' => not_implemented!(fxb@319745106, "SysRqOomKiller"),
                b'h' => not_implemented!(fxb@319745106, "SysRqPrintHelp"),
                b'i' => not_implemented!(fxb@319745106, "SysRqSigkillAllButInit"),
                b'j' => not_implemented!(fxb@319745106, "SysRqJustThawIt"),
                b'k' => not_implemented!(fxb@319745106, "SysRqSecureAccessKey"),
                b'l' => not_implemented!(fxb@319745106, "SysRqBacktraceActiveCpus"),
                b'm' => not_implemented!(fxb@319745106, "SysRqDumpMemoryInfo"),
                b'n' => not_implemented!(fxb@319745106, "SysRqRealtimeNice"),
                b'o' => not_implemented!(fxb@319745106, "SysRqPowerOff"),
                b'p' => not_implemented!(fxb@319745106, "SysRqDumpRegistersAndFlags"),
                b'q' => not_implemented!(fxb@319745106, "SysRqDumpHrTimers"),
                b'r' => not_implemented!(fxb@319745106, "SysRqDisableKeyboardRawMode"),
                b's' => not_implemented!(fxb@319745106, "SysRqSyncMountedFilesystems"),
                b't' => not_implemented!(fxb@319745106, "SysRqDumpCurrentTasks"),
                b'u' => not_implemented!(fxb@319745106, "SysRqRemountAllReadonly"),
                b'v' => not_implemented!(fxb@319745106, "SysRqRestoreFramebuffer"),
                b'w' => not_implemented!(fxb@319745106, "SysRqDumpBlockedTasks"),
                b'x' => not_implemented!(fxb@319745106, "SysRqDumpFtraceBuffer"),
                b'0' => not_implemented!(fxb@319745106, "SysRqLogLevel0"),
                b'1' => not_implemented!(fxb@319745106, "SysRqLogLevel1"),
                b'2' => not_implemented!(fxb@319745106, "SysRqLogLevel2"),
                b'3' => not_implemented!(fxb@319745106, "SysRqLogLevel3"),
                b'4' => not_implemented!(fxb@319745106, "SysRqLogLevel4"),
                b'5' => not_implemented!(fxb@319745106, "SysRqLogLevel5"),
                b'6' => not_implemented!(fxb@319745106, "SysRqLogLevel6"),
                b'7' => not_implemented!(fxb@319745106, "SysRqLogLevel7"),
                b'8' => not_implemented!(fxb@319745106, "SysRqLogLevel8"),
                b'9' => not_implemented!(fxb@319745106, "SysRqLogLevel9"),

                _ => return error!(EINVAL),
            }
        }
        Ok(commands.len())
    }

    fn seek(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _current_offset: off_t,
        _target: SeekTarget,
    ) -> Result<off_t, Errno> {
        error!(EINVAL)
    }
}
