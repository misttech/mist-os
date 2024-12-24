// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::DeviceMetadata;
use crate::device::{simple_device_ops, DeviceMode};
use crate::fs::sysfs::DeviceDirectory;
use crate::mm::{
    create_anonymous_mapping_memory, DesiredAddress, MappingName, MappingOptions,
    MemoryAccessorExt, ProtectionFlags,
};
use crate::task::syslog::{self, KmsgLevel};
use crate::task::{CurrentTask, EventHandler, LogSubscription, Syslog, WaitCanceler, Waiter};
use crate::vfs::buffers::{InputBuffer, InputBufferExt as _, OutputBuffer};
use crate::vfs::{
    fileops_impl_noop_sync, fileops_impl_seekless, Anon, FileHandle, FileObject, FileOps,
    FileWriteGuardRef, FsNode, FsNodeInfo, NamespaceNode, SeekTarget,
};
use starnix_logging::{log, log_info, track_stub, Level};
use starnix_sync::{DeviceOpen, FileOpsCore, LockBefore, Locked, Mutex, Unlocked};
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::FileMode;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::user_address::UserAddress;
use starnix_uapi::vfs::FdEvents;
use starnix_uapi::{errno, error};
use std::mem::MaybeUninit;
use zx::{
    cprng_draw_uninit, {self as zx},
};

#[derive(Default)]
pub struct DevNull;

pub fn new_null_file(current_task: &CurrentTask, flags: OpenFlags) -> FileHandle {
    Anon::new_file_extended(
        current_task,
        Box::new(DevNull),
        flags,
        "[fuchsia:null]",
        FsNodeInfo::new_factory(FileMode::from_bits(0o666), FsCred::root()),
    )
}

impl FileOps for DevNull {
    fileops_impl_seekless!();
    fileops_impl_noop_sync!();

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        // Writes to /dev/null on Linux treat the input buffer in an unconventional way. The actual
        // data is not touched and if the input parameters are plausible the device claims to
        // successfully write up to MAX_RW_COUNT bytes.  If the input parameters are outside of the
        // user accessible address space, writes will return EFAULT.

        // For debugging log up to 4096 bytes from the input buffer. We don't care about errors when
        // trying to read data to log. The amount of data logged is chosen arbitrarily.
        let bytes_to_log = std::cmp::min(4096, data.available());
        let log_buffer = data.read_to_vec_limited(bytes_to_log);
        let bytes_logged = match log_buffer {
            Ok(bytes) => {
                if cfg!(feature = "starnix_log_dev_null_writes_at_info") {
                    log_info!("write to devnull: {:?}", String::from_utf8_lossy(&bytes));
                }
                bytes.len()
            }
            Err(_) => 0,
        };

        Ok(bytes_logged + data.drain())
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        Ok(0)
    }

    fn to_handle(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<Option<zx::Handle>, Errno> {
        Ok(None)
    }
}

#[derive(Default)]
struct DevZero;
impl FileOps for DevZero {
    fileops_impl_seekless!();
    fileops_impl_noop_sync!();

    fn mmap(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        addr: DesiredAddress,
        memory_offset: u64,
        length: usize,
        prot_flags: ProtectionFlags,
        mut options: MappingOptions,
        filename: NamespaceNode,
    ) -> Result<UserAddress, Errno> {
        // All /dev/zero mappings behave as anonymous mappings.
        //
        // This means that we always create a new zero-filled VMO for this mmap request.
        // Memory is never shared between two mappings of /dev/zero, even if
        // `MappingOptions::SHARED` is set.
        //
        // Similar to anonymous mappings, if this process were to request a shared mapping
        // of /dev/zero and then fork, the child and the parent process would share the
        // VMO created here.
        let memory = create_anonymous_mapping_memory(length as u64)?;

        options |= MappingOptions::ANONYMOUS;

        current_task.mm().ok_or_else(|| errno!(EINVAL))?.map_memory(
            addr,
            memory,
            memory_offset,
            length,
            prot_flags,
            file.max_access_for_memory_mapping(),
            options,
            // We set the filename here, even though we are creating what is
            // functionally equivalent to an anonymous mapping. Doing so affects
            // the output of `/proc/self/maps` and identifies this mapping as
            // file-based.
            MappingName::File(filename.into_active()),
            FileWriteGuardRef(None),
        )
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        Ok(data.drain())
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        data.zero()
    }
}

#[derive(Default)]
struct DevFull;
impl FileOps for DevFull {
    fileops_impl_seekless!();
    fileops_impl_noop_sync!();

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(ENOSPC)
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        data.write_each(&mut |bytes| {
            bytes.fill(MaybeUninit::new(0));
            Ok(bytes.len())
        })
    }
}

#[derive(Default)]
pub struct DevRandom;
impl FileOps for DevRandom {
    fileops_impl_seekless!();
    fileops_impl_noop_sync!();

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        Ok(data.drain())
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        data.write_each(&mut |bytes| {
            let read_bytes = cprng_draw_uninit(bytes);
            Ok(read_bytes.len())
        })
    }

    fn ioctl(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: starnix_syscalls::SyscallArg,
    ) -> Result<starnix_syscalls::SyscallResult, Errno> {
        match request {
            starnix_uapi::RNDGETENTCNT => {
                let addr = starnix_uapi::user_address::UserRef::<i32>::new(UserAddress::from(arg));
                // Linux just returns 256 no matter what (as observed on 6.5.6).
                let result = 256;
                current_task.write_object(addr, &result).map(|_| starnix_syscalls::SUCCESS)
            }
            _ => crate::vfs::default_ioctl(file, locked, current_task, request, arg),
        }
    }
}

pub fn open_kmsg(
    _locked: &mut Locked<'_, DeviceOpen>,
    current_task: &CurrentTask,
    _id: DeviceType,
    _node: &FsNode,
    flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    Syslog::validate_access(current_task)?;
    let subscription = if flags.can_read() {
        Some(Mutex::new(Syslog::snapshot_then_subscribe(&current_task)?))
    } else {
        None
    };
    Ok(Box::new(DevKmsg(subscription)))
}

struct DevKmsg(Option<Mutex<LogSubscription>>);

impl FileOps for DevKmsg {
    fileops_impl_noop_sync!();

    fn has_persistent_offsets(&self) -> bool {
        false
    }

    fn is_seekable(&self) -> bool {
        true
    }

    fn seek(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &crate::vfs::FileObject,
        current_task: &crate::task::CurrentTask,
        _current_offset: starnix_uapi::off_t,
        target: crate::vfs::SeekTarget,
    ) -> Result<starnix_uapi::off_t, starnix_uapi::errors::Errno> {
        match target {
            SeekTarget::Set(0) => {
                let Some(ref subscription) = self.0 else {
                    return Ok(0);
                };
                let mut guard = subscription.lock();
                *guard = Syslog::snapshot_then_subscribe(current_task)?;
                Ok(0)
            }
            SeekTarget::End(0) => {
                let Some(ref subscription) = self.0 else {
                    return Ok(0);
                };
                let mut guard = subscription.lock();
                *guard = Syslog::subscribe(current_task)?;
                Ok(0)
            }
            SeekTarget::Data(0) => {
                track_stub!(TODO("https://fxbug.dev/322874315"), "/dev/kmsg: SEEK_DATA");
                Ok(0)
            }
            // The following are implemented as documented on:
            // https://www.kernel.org/doc/Documentation/ABI/testing/dev-kmsg
            // The only accepted seek targets are "SEEK_END,0", "SEEK_SET,0" and "SEEK_DATA,0"
            // When given an invalid offset, ESPIPE is expected.
            SeekTarget::End(_) | SeekTarget::Set(_) | SeekTarget::Data(_) => {
                error!(ESPIPE, "Unsupported offset")
            }
            // According to the docs above and observations, this should be EINVAL, but dprintf
            // fails if we make it EINVAL.
            SeekTarget::Cur(_) => error!(ESPIPE),
            SeekTarget::Hole(_) => error!(EINVAL, "Unsupported seek target"),
        }
    }

    fn wait_async(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        self.0.as_ref().map(|subscription| subscription.lock().wait(waiter, events, handler))
    }

    fn query_events(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        let mut events = FdEvents::empty();
        if let Some(subscription) = self.0.as_ref() {
            if subscription.lock().available()? > 0 {
                events |= FdEvents::POLLIN;
            }
        }
        Ok(events)
    }

    fn read(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        file.blocking_op(locked, current_task, FdEvents::POLLIN | FdEvents::POLLHUP, None, |_| {
            match self.0.as_ref().unwrap().lock().next() {
                Some(Ok(log)) => data.write(&log),
                Some(Err(err)) => Err(err),
                None => Ok(0),
            }
        })
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let bytes = data.read_all()?;
        let extract_result = syslog::extract_level(&bytes);
        let (level, msg_bytes) = match extract_result {
            None => (Level::Info, bytes.as_slice()),
            Some((level, bytes_after_level)) => match level {
                // An error but keep the <level> str.
                KmsgLevel::Emergency | KmsgLevel::Alert | KmsgLevel::Critical => {
                    (Level::Error, bytes.as_slice())
                }
                KmsgLevel::Error => (Level::Error, bytes_after_level),
                KmsgLevel::Warning => (Level::Warn, bytes_after_level),
                // Log as info but show the <level>.
                KmsgLevel::Notice => (Level::Info, bytes.as_slice()),
                KmsgLevel::Info => (Level::Info, bytes_after_level),
                KmsgLevel::Debug => (Level::Debug, bytes_after_level),
            },
        };
        log!(level, tag = "kmsg"; "{}", String::from_utf8_lossy(msg_bytes).trim_end_matches('\n'));
        Ok(bytes.len())
    }
}

pub fn mem_device_init<L>(locked: &mut Locked<'_, L>, system_task: &CurrentTask)
where
    L: LockBefore<FileOpsCore>,
{
    let kernel = system_task.kernel();
    let registry = &kernel.device_registry;

    let mem_class = registry.objects.mem_class();
    registry.register_device(
        locked,
        system_task,
        "null".into(),
        DeviceMetadata::new("null".into(), DeviceType::NULL, DeviceMode::Char),
        mem_class.clone(),
        DeviceDirectory::new,
        simple_device_ops::<DevNull>,
    );
    registry.register_device(
        locked,
        system_task,
        "zero".into(),
        DeviceMetadata::new("zero".into(), DeviceType::ZERO, DeviceMode::Char),
        mem_class.clone(),
        DeviceDirectory::new,
        simple_device_ops::<DevZero>,
    );
    registry.register_device(
        locked,
        system_task,
        "full".into(),
        DeviceMetadata::new("full".into(), DeviceType::FULL, DeviceMode::Char),
        mem_class.clone(),
        DeviceDirectory::new,
        simple_device_ops::<DevFull>,
    );
    registry.register_device(
        locked,
        system_task,
        "random".into(),
        DeviceMetadata::new("random".into(), DeviceType::RANDOM, DeviceMode::Char),
        mem_class.clone(),
        DeviceDirectory::new,
        simple_device_ops::<DevRandom>,
    );
    registry.register_device(
        locked,
        system_task,
        "urandom".into(),
        DeviceMetadata::new("urandom".into(), DeviceType::URANDOM, DeviceMode::Char),
        mem_class.clone(),
        DeviceDirectory::new,
        simple_device_ops::<DevRandom>,
    );
    registry.register_device(
        locked,
        system_task,
        "kmsg".into(),
        DeviceMetadata::new("kmsg".into(), DeviceType::KMSG, DeviceMode::Char),
        mem_class,
        DeviceDirectory::new,
        open_kmsg,
    );
}
