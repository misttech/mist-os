// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{borrow::Cow, sync::Arc};

use super::*;
use crate::{
    fs::{
        buffers::{InputBuffer, OutputBuffer},
        fileops_impl_seekable,
    },
    task::*,
    types::{as_any::AsAny, *},
};

pub struct SimpleFileNode<F, O>
where
    F: Fn() -> Result<O, Errno>,
    O: FileOps,
{
    create_file_ops: F,
}

impl<F, O> SimpleFileNode<F, O>
where
    F: Fn() -> Result<O, Errno> + Send + Sync,
    O: FileOps,
{
    pub fn new(create_file_ops: F) -> SimpleFileNode<F, O> {
        SimpleFileNode { create_file_ops }
    }
}

impl<F, O> FsNodeOps for SimpleFileNode<F, O>
where
    F: Fn() -> Result<O, Errno> + Send + Sync + 'static,
    O: FileOps,
{
    fn create_file_ops(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new((self.create_file_ops)()?))
    }

    fn truncate(
        &self,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _length: u64,
    ) -> Result<(), Errno> {
        // TODO(tbodt): Is this right? This is the minimum to handle O_TRUNC
        Ok(())
    }
}

pub fn parse_u32_file(buf: &[u8]) -> Result<u32, Errno> {
    let i = buf.iter().position(|c| !char::from(*c).is_ascii_digit()).unwrap_or(buf.len());
    std::str::from_utf8(&buf[..i]).unwrap().parse::<u32>().map_err(|_| errno!(EINVAL))
}

pub fn serialize_u32_file(value: u32) -> Vec<u8> {
    let string = format!("{}\n", value);
    string.as_bytes().to_vec()
}

pub fn parse_i32_file(buf: &[u8]) -> Result<i32, Errno> {
    let i = buf
        .iter()
        .position(|c| {
            let ch = char::from(*c);
            !(ch.is_ascii_digit() || ch == '-')
        })
        .unwrap_or(buf.len());
    std::str::from_utf8(&buf[..i]).unwrap().parse::<i32>().map_err(|_| errno!(EINVAL))
}

pub fn serialize_i32_file(value: i32) -> Vec<u8> {
    let string = format!("{}\n", value);
    string.as_bytes().to_vec()
}

pub struct BytesFile<Ops: BytesFileOps>(Arc<Ops>);

impl<Ops: BytesFileOps> BytesFile<Ops> {
    pub fn new_node(data: Ops) -> impl FsNodeOps {
        let data = Arc::new(data);
        SimpleFileNode::new(move || Ok(BytesFile(Arc::clone(&data))))
    }
}

impl<Ops: BytesFileOps> FileOps for BytesFile<Ops> {
    fileops_impl_seekable!();

    fn read(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        data.write(&self.0.read(current_task)?[offset..])
    }

    fn write(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        if offset != 0 {
            // TODO: Validate whether this error condition is correct.
            // It doesn't appear to be correct for /proc/<pid>/oom_*
            return error!(EINVAL);
        }
        let data = data.read_all()?;
        let len = data.len();
        self.0.write(current_task, data)?;
        Ok(len)
    }
}

pub trait BytesFileOps: Send + Sync + AsAny + 'static {
    fn write(&self, _current_task: &CurrentTask, _data: Vec<u8>) -> Result<(), Errno> {
        error!(ENOSYS)
    }
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        error!(ENOSYS)
    }
}

impl BytesFileOps for Vec<u8> {
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(self.into())
    }
}
