// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mm::{FaultRegisterMode, MemoryAccessorExt, UserFault, UserFaultFeatures};
use crate::task::{CurrentTask, EventHandler, WaitCanceler, Waiter};
use crate::vfs::{
    fileops_impl_nonseekable, fileops_impl_noop_sync, Anon, FileHandle, FileObject, FileOps,
    InputBuffer, OutputBuffer,
};
use linux_uapi::{UFFDIO_CONTINUE, UFFDIO_COPY, UFFDIO_WAKE, UFFDIO_WRITEPROTECT, UFFDIO_ZEROPAGE};
use starnix_logging::track_stub;
use starnix_sync::{FileOpsCore, LockBefore, Locked, Unlocked, UserFaultInner};
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::user_address::UserRef;
use starnix_uapi::vfs::FdEvents;
use starnix_uapi::{
    errno, error, uffdio_api, uffdio_range, uffdio_register, UFFDIO, UFFDIO_API, UFFDIO_MOVE,
    UFFDIO_POISON, UFFDIO_REGISTER, UFFDIO_UNREGISTER, _UFFDIO_API, _UFFDIO_REGISTER,
    _UFFDIO_UNREGISTER,
};
use static_assertions::const_assert_eq;
use std::sync::Arc;

pub struct UserFaultFile {
    inner: Arc<UserFault>,
}

// API version hasn't changed
const_assert_eq!(UFFDIO, 0xAA);

impl UserFaultFile {
    pub fn new(
        current_task: &CurrentTask,
        open_flags: OpenFlags,
        _user_mode_only: bool,
    ) -> FileHandle {
        let mm = current_task.mm().unwrap();
        let inner = Arc::new(UserFault::new(Arc::downgrade(&mm)));
        Anon::new_file(current_task, Box::new(Self { inner }), open_flags, "[userfaultfd]")
    }

    fn api_handshake<L>(
        &self,
        locked: &mut Locked<'_, L>,
        _current_task: &CurrentTask,
        request: uffdio_api,
    ) -> Result<uffdio_api, Errno>
    where
        L: LockBefore<UserFaultInner>,
    {
        if self.inner.is_initialized(locked) {
            return error!(EPERM, "userfault object already initialized");
        }

        if request.api != UFFDIO as u64 {
            return error!(EINVAL, format!("unsupported API version {}", request.api));
        }

        let requested_features =
            UserFaultFeatures::from_bits(request.features.try_into().map_err(|_| errno!(EINVAL))?)
                .ok_or_else(|| errno!(EINVAL))?;
        let requested_unsupported = requested_features.difference(UserFaultFeatures::ALL_SUPPORTED);
        if !requested_unsupported.is_empty() {
            return error!(EINVAL);
        }

        // We can support the client, initialize the object.
        self.inner.initialize(locked, requested_features);

        Ok(uffdio_api {
            api: request.api,
            features: UserFaultFeatures::ALL_SUPPORTED.bits() as u64,
            ioctls: (1 << _UFFDIO_API) | (1 << _UFFDIO_REGISTER) | (1 << _UFFDIO_UNREGISTER),
        })
    }
}

impl FileOps for UserFaultFile {
    fileops_impl_nonseekable!();
    fileops_impl_noop_sync!();
    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        track_stub!(TODO("https://fxbug.dev/391599171"), "event-based uffd operations");
        error!(ENOTSUP)
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(EINVAL)
    }

    fn query_events(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        track_stub!(TODO("https://fxbug.dev/391599171"), "event-based uffd operations");
        error!(ENOTSUP)
    }

    fn wait_async(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _waiter: &Waiter,
        _events: FdEvents,
        _handler: EventHandler,
    ) -> Option<WaitCanceler> {
        track_stub!(TODO("https://fxbug.dev/391599171"), "event-based uffd operations");
        None
    }

    fn ioctl(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        _file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: starnix_syscalls::SyscallArg,
    ) -> Result<starnix_syscalls::SyscallResult, Errno> {
        match request {
            UFFDIO_API => {
                let arg: UserRef<uffdio_api> = arg.into();
                let request = current_task.read_object(arg)?;
                match self.api_handshake(locked, current_task, request) {
                    Ok(reply) => {
                        current_task.write_object(arg, &reply)?;
                        Ok(0.into())
                    }
                    Err(e) => {
                        current_task.write_object(arg, &uffdio_api::default())?;
                        Err(e)
                    }
                }
            }

            UFFDIO_REGISTER => {
                let arg: UserRef<uffdio_register> = arg.into();
                let mut request = current_task.read_object(arg)?;

                request.ioctls = self
                    .inner
                    .op_register(
                        locked,
                        request.range.start.into(),
                        request.range.len,
                        FaultRegisterMode::from_bits_truncate(
                            request.mode.try_into().map_err(|_| errno!(EINVAL))?,
                        ),
                    )?
                    .bits();
                current_task.write_object(arg, &request)?;
                Ok(0.into())
            }

            UFFDIO_UNREGISTER => {
                let arg: UserRef<uffdio_range> = arg.into();
                let request = current_task.read_object(arg)?;
                self.inner.op_unregister(locked, request.start.into(), request.len)?;
                Ok(0.into())
            }

            UFFDIO_COPY | UFFDIO_ZEROPAGE | UFFDIO_MOVE => {
                track_stub!(TODO("https://fxbug.dev/297375964"), "basic uffd ioctls", request);
                error!(ENOSYS)
            }

            UFFDIO_WAKE | UFFDIO_WRITEPROTECT | UFFDIO_CONTINUE | UFFDIO_POISON => {
                track_stub!(
                    TODO("https://fxbug.dev/322893681"),
                    "full set of uffd ioctls",
                    request
                );
                error!(ENOSYS)
            }

            unknown => error!(EINVAL, format!("unknown ioctl request {unknown}")),
        }
    }

    // On closing, clear all the registrations pointing to this userfault object.
    fn close(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) {
        self.inner.cleanup();
    }
}
