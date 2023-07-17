// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    auth::FsCred,
    device::{simple_device_ops, DeviceMode, DeviceOps},
    fs::{
        buffers::{InputBuffer, OutputBuffer},
        kobject::{KObjectDeviceAttribute, KType},
        sysfs::SysFsDirectory,
        *,
    },
    logging::*,
    mm::*,
    task::*,
    types::*,
};

use fuchsia_zircon::{self as zx, cprng_draw};
use std::sync::Arc;

#[derive(Default)]
pub struct DevNull;

pub fn new_null_file(kernel: &Arc<Kernel>, flags: OpenFlags) -> FileHandle {
    Anon::new_file_extended(
        kernel,
        Box::new(DevNull),
        flags,
        FsNodeInfo::new_factory(FileMode::from_bits(0o666), FsCred::root()),
    )
}

impl FileOps for DevNull {
    fileops_impl_seekless!();

    fn write(
        &self,
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
        let mut log_buffer = vec![0; bytes_to_log];
        let bytes_logged = match data.read(&mut log_buffer) {
            Ok(bytes) => {
                log_info!("write to devnull: {:?}", String::from_utf8_lossy(&log_buffer[0..bytes]));
                bytes
            }
            Err(_) => 0,
        };
        Ok(bytes_logged + data.drain())
    }

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        Ok(0)
    }

    fn to_handle(
        &self,
        _file: &FileHandle,
        _current_task: &CurrentTask,
    ) -> Result<Option<zx::Handle>, Errno> {
        Ok(None)
    }
}

#[derive(Default)]
struct DevZero;
impl FileOps for DevZero {
    fileops_impl_seekless!();

    fn mmap(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        addr: DesiredAddress,
        vmo_offset: u64,
        length: usize,
        prot_flags: ProtectionFlags,
        mut options: MappingOptions,
        filename: NamespaceNode,
    ) -> Result<MappedVmo, Errno> {
        // All /dev/zero mappings behave as anonymous mappings.
        //
        // This means that we always create a new zero-filled VMO for this mmap request.
        // Memory is never shared between two mappings of /dev/zero, even if
        // `MappingOptions::SHARED` is set.
        //
        // Similar to anonymous mappings, if this process were to request a shared mapping
        // of /dev/zero and then fork, the child and the parent process would share the
        // VMO created here.
        let vmo = create_anonymous_mapping_vmo(length as u64)?;

        options |= MappingOptions::ANONYMOUS;

        let addr = current_task.mm.map(
            addr,
            vmo.clone(),
            vmo_offset,
            length,
            prot_flags,
            options,
            // We set the filename here, even though we are creating what is
            // functionally equivalent to an anonymous mapping. Doing so affects
            // the output of `/proc/self/maps` and identifies this mapping as
            // file-based.
            MappingName::File(filename),
            FileWriteGuardRef(None),
        )?;
        Ok(MappedVmo::new(vmo, addr))
    }

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        Ok(data.drain())
    }

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        data.write_each(&mut |bytes| {
            bytes.fill(0);
            Ok(bytes.len())
        })
    }
}

#[derive(Default)]
struct DevFull;
impl FileOps for DevFull {
    fileops_impl_seekless!();

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(ENOSPC)
    }

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        data.write_each(&mut |bytes| {
            bytes.fill(0);
            Ok(bytes.len())
        })
    }
}

#[derive(Default)]
pub struct DevRandom;
impl FileOps for DevRandom {
    fileops_impl_seekless!();

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        Ok(data.drain())
    }

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        data.write_each(&mut |bytes| {
            cprng_draw(bytes);
            Ok(bytes.len())
        })
    }
}

#[derive(Default)]
struct DevKmsg;
impl FileOps for DevKmsg {
    fileops_impl_seekless!();

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        Ok(0)
    }

    fn write(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let bytes = data.read_all()?;
        log!(
            level = info,
            tag = "kmsg",
            "{}",
            String::from_utf8_lossy(&bytes).trim_end_matches('\n')
        );
        Ok(bytes.len())
    }
}

fn create_mem_device(
    kernel: &Arc<Kernel>,
    name: &FsStr,
    device_type: DeviceType,
    device_ops: impl DeviceOps,
) {
    let mem_class = kernel.device_registry.virtual_bus().get_or_create_child(
        b"mem",
        KType::Class,
        SysFsDirectory::new,
    );
    let device_attr = KObjectDeviceAttribute::new(name, name, device_type, DeviceMode::Char);
    kernel.add_chr_device(mem_class, device_attr);
    kernel
        .device_registry
        .register_chrdev(MEM_MAJOR, device_type.minor(), 1, device_ops)
        .expect("mem device register failed.");
}

pub fn mem_device_init(kernel: &Arc<Kernel>) {
    create_mem_device(kernel, b"null", DeviceType::NULL, simple_device_ops::<DevNull>);
    create_mem_device(kernel, b"zero", DeviceType::ZERO, simple_device_ops::<DevZero>);
    create_mem_device(kernel, b"full", DeviceType::FULL, simple_device_ops::<DevFull>);
    create_mem_device(kernel, b"random", DeviceType::RANDOM, simple_device_ops::<DevRandom>);
    create_mem_device(kernel, b"urandom", DeviceType::URANDOM, simple_device_ops::<DevRandom>);
    create_mem_device(kernel, b"kmsg", DeviceType::KMSG, simple_device_ops::<DevKmsg>);
}
