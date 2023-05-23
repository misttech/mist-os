// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::borrow::Cow;
use std::sync::Arc;

use super::*;
use crate::fs::buffers::{InputBuffer, OutputBuffer};
use crate::fs::fileops_impl_seekable;
use crate::task::*;
use crate::types::as_any::AsAny;
use crate::types::*;

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
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new((self.create_file_ops)()?))
    }

    fn truncate(&self, _node: &FsNode, _length: u64) -> Result<(), Errno> {
        // TODO(tbodt): Is this right? This is the minimum to handle O_TRUNC
        Ok(())
    }
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
