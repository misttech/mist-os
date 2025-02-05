// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! See https://www.kernel.org/doc/html/latest/admin-guide/sysrq.html.

use crate::task::{CurrentTask, Kernel};
use crate::vfs::{
    fileops_impl_noop_sync, AppendLockGuard, FileObject, FileOps, FsNode, FsNodeHandle, FsNodeOps,
    FsStr, InputBuffer, OutputBuffer, SeekTarget,
};
use fidl_fuchsia_hardware_power_statecontrol::{AdminMarker, RebootOptions, RebootReason2};
use fuchsia_component::client::connect_to_protocol_sync;
use starnix_logging::{log_info, log_warn, track_stub};
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::FileMode;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{error, off_t};

pub struct SysRqNode {}

impl SysRqNode {
    pub fn new(_kernel: &Kernel) -> Self {
        Self {}
    }
}

impl FsNodeOps for SysRqNode {
    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(SysRqFile {}))
    }

    fn mknod(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
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
        _locked: &mut Locked<'_, FileOpsCore>,
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
        _locked: &mut Locked<'_, FileOpsCore>,
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
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        error!(EINVAL)
    }

    fn truncate(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _guard: &AppendLockGuard<'_>,
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
    fileops_impl_noop_sync!();

    fn is_seekable(&self) -> bool {
        false
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        error!(EINVAL)
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let commands = data.read_all()?;
        for command in &commands {
            match *command {
                b'b' => track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqRebootNoSync"),
                b'c' => {
                    log_warn!("SysRq kernel crash request.");

                    // Attempt to "crash" the whole device, if that fails settle for just Starnix.
                    // When this call succeeds with a production implementation it should never
                    // return. If it returns at all it is a sign the kernel either doesn't have the
                    // capability or there was a problem with the shutdown request.
                    let reboot_res =
                        connect_to_protocol_sync::<AdminMarker>().unwrap().perform_reboot(
                            &RebootOptions {
                                reasons: Some(vec![RebootReason2::CriticalComponentFailure]),
                                ..Default::default()
                            },
                            zx::MonotonicInstant::INFINITE,
                        );

                    // LINT.IfChange
                    panic!(
                        "reboot call returned unexpectedly ({:?}), crashing from SysRq",
                        reboot_res
                    );
                    // LINT.ThenChange(src/starnix/tests/sysrq/src/lib.rs)
                }
                b'd' => track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqDumpLocksHeld"),
                b'e' => {
                    track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqSigtermAllButInit")
                }
                b'f' => track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqOomKiller"),
                b'h' => track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqPrintHelp"),
                b'i' => {
                    track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqSigkillAllButInit")
                }
                b'j' => track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqJustThawIt"),
                b'k' => {
                    track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqSecureAccessKey")
                }
                b'l' => {
                    track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqBacktraceActiveCpus",)
                }
                b'm' => {
                    track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqDumpMemoryInfo")
                }
                b'n' => track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqRealtimeNice"),
                b'o' => {
                    log_info!("SysRq kernel shutdown request.");
                    current_task.kernel().shut_down();
                }
                b'p' => {
                    track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqDumpRegistersAndFlags",)
                }
                b'q' => track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqDumpHrTimers"),
                b'r' => {
                    track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqDisableKeyboardRawMode",)
                }
                b's' => {
                    track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqSyncMountedFilesystems",)
                }
                b't' => {
                    track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqDumpCurrentTasks")
                }
                b'u' => {
                    track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqRemountAllReadonly")
                }
                b'v' => {
                    track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqRestoreFramebuffer")
                }
                b'w' => {
                    track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqDumpBlockedTasks")
                }
                b'x' => {
                    track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqDumpFtraceBuffer")
                }
                b'0' => track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqLogLevel0"),
                b'1' => track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqLogLevel1"),
                b'2' => track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqLogLevel2"),
                b'3' => track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqLogLevel3"),
                b'4' => track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqLogLevel4"),
                b'5' => track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqLogLevel5"),
                b'6' => track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqLogLevel6"),
                b'7' => track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqLogLevel7"),
                b'8' => track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqLogLevel8"),
                b'9' => track_stub!(TODO("https://fxbug.dev/319745106"), "SysRqLogLevel9"),

                _ => return error!(EINVAL),
            }
        }
        Ok(commands.len())
    }

    fn seek(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _current_offset: off_t,
        _target: SeekTarget,
    ) -> Result<off_t, Errno> {
        error!(EINVAL)
    }
}
