// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon::{self as zx, cprng_draw};

use crate::auth::FsCred;
use crate::device::DeviceOps;
use crate::fs::buffers::{InputBuffer, OutputBuffer};
use crate::fs::*;
use crate::logging::*;
use crate::task::*;
use crate::types::*;

struct DevNull;

pub fn new_null_file(kernel: &Kernel, flags: OpenFlags) -> FileHandle {
    Anon::new_file_extended(
        kernel,
        Box::new(DevNull),
        FileMode::from_bits(0o666),
        FsCred::root(),
        flags,
    )
}

impl FileOps for DevNull {
    fileops_impl_seekless!();

    fn write_at(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        data.read_each(&mut |bytes| {
            log_info!(current_task, "write to devnull: {:?}", String::from_utf8_lossy(bytes));
            Ok(bytes.len())
        })
    }

    fn read_at(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        Ok(0)
    }

    fn to_handle(&self, _file: &FileHandle, _kernel: &Kernel) -> Result<Option<zx::Handle>, Errno> {
        Ok(None)
    }
}

struct DevZero;
impl FileOps for DevZero {
    fileops_impl_seekless!();

    fn write_at(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        Ok(data.drain())
    }

    fn read_at(
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

struct DevFull;
impl FileOps for DevFull {
    fileops_impl_seekless!();

    fn write_at(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        error!(ENOSPC)
    }

    fn read_at(
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

struct DevRandom;
impl FileOps for DevRandom {
    fileops_impl_seekless!();

    fn write_at(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        Ok(data.drain())
    }

    fn read_at(
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

struct DevKmsg;
impl FileOps for DevKmsg {
    fileops_impl_seekless!();

    fn read_at(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        Ok(0)
    }

    fn write_at(
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

pub struct MemDevice;
impl DeviceOps for MemDevice {
    fn open(
        &self,
        _current_task: &CurrentTask,
        id: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(match id.minor() {
            3 => Box::new(DevNull),
            5 => Box::new(DevZero),
            7 => Box::new(DevFull),
            8 | 9 => Box::new(DevRandom),
            11 => Box::new(DevKmsg),
            _ => return error!(ENODEV),
        })
    }
}

pub struct MiscDevice;
impl DeviceOps for MiscDevice {
    fn open(
        &self,
        _current_task: &CurrentTask,
        id: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(match id.minor() {
            183 => Box::new(DevRandom),
            _ => return error!(ENODEV),
        })
    }
}
