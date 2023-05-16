// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        fs::{
            buffers::{InputBuffer, InputBufferCallback},
            pipe::PipeFileObject,
            FdNumber,
        },
        logging::not_implemented,
        mm::{ProtectionFlags, PAGE_SIZE},
        task::CurrentTask,
        types::{errno, error, off_t, Errno, OpenFlags, UserAddress, UserRef, MAX_RW_COUNT},
    },
    fuchsia_zircon as zx,
    std::{sync::Arc, usize},
};

/// An input buffer that reads from a VMO in PAGE_SIZE segments
/// Used by the `sendfile` syscall.
#[derive(Debug)]
struct SendFileBuffer {
    vmo: Arc<zx::Vmo>,
    start_pos: usize,
    cur_pos: usize,
    end_pos: usize,
}

impl InputBuffer for SendFileBuffer {
    fn peek_each(&mut self, callback: &mut InputBufferCallback<'_>) -> Result<usize, Errno> {
        let mut peek_pos = self.cur_pos;

        while peek_pos < self.end_pos {
            let peekable = self.end_pos - peek_pos;

            // Only read at most a page worth of data from the VMO.
            let segment = std::cmp::min(peekable, *PAGE_SIZE as usize);
            let data =
                self.vmo.read_to_vec(peek_pos as u64, segment as u64).map_err(|_| errno!(EIO))?;

            let bytes_peeked = callback(data.as_slice())?;
            peek_pos += bytes_peeked;

            if bytes_peeked < data.len() {
                break;
            }
        }

        Ok(peek_pos - self.cur_pos)
    }

    fn available(&self) -> usize {
        assert!(self.end_pos >= self.cur_pos);
        self.end_pos - self.cur_pos
    }

    fn bytes_read(&self) -> usize {
        assert!(self.cur_pos >= self.start_pos);
        self.cur_pos - self.start_pos
    }

    fn drain(&mut self) -> usize {
        let bytes_swallowed = self.available();
        self.cur_pos = self.end_pos;
        bytes_swallowed
    }

    fn advance(&mut self, length: usize) -> Result<(), Errno> {
        self.cur_pos += length;
        assert!(self.end_pos >= self.cur_pos);
        Ok(())
    }
}

pub fn sendfile(
    current_task: &CurrentTask,
    out_fd: FdNumber,
    in_fd: FdNumber,
    offset: UserAddress,
    count: i32,
) -> Result<usize, Errno> {
    if !offset.is_null() {
        not_implemented!("sendfile non-null offset");
        return error!(ENOSYS);
    }

    if count < 0 {
        return error!(EINVAL);
    }

    let out_file = current_task.files.get(out_fd)?;
    let in_file = current_task.files.get(in_fd)?;

    if !in_file.flags().can_read() || !out_file.flags().can_write() {
        return error!(EBADF);
    }

    // out_fd has the O_APPEND flag set.  This is not currently supported by sendfile().
    // See https://man7.org/linux/man-pages/man2/sendfile.2.html#ERRORS
    if out_file.flags().contains(OpenFlags::APPEND) {
        return error!(EINVAL);
    }

    let count = count as usize;
    let count = std::cmp::min(count, *MAX_RW_COUNT);

    // Lock the in_file offset and info for the entire operation.
    let mut in_offset = in_file.offset.lock();
    let info = in_file.node().info();

    let vmo =
        in_file.get_vmo(current_task, None, ProtectionFlags::READ).map_err(|_| errno!(EINVAL))?;

    assert!(*in_offset >= 0);
    let start_pos = *in_offset as usize;
    let end_pos = std::cmp::min(start_pos + count, info.size);

    if start_pos >= end_pos {
        return Ok(0);
    }

    let mut buffer = SendFileBuffer { vmo, start_pos, cur_pos: start_pos, end_pos };

    let num_bytes_written = out_file.write(current_task, &mut buffer)?;

    // Increase the in file offset by the number of bytes actually written.
    *in_offset += num_bytes_written as i64;

    Ok(num_bytes_written)
}

pub fn splice(
    current_task: &CurrentTask,
    fd_in: FdNumber,
    _off_in: UserRef<off_t>,
    fd_out: FdNumber,
    _off_out: UserRef<off_t>,
    _len: usize,
    _flags: u32,
) -> Result<usize, Errno> {
    let in_file = current_task.files.get(fd_in)?;
    let out_file = current_task.files.get(fd_out)?;

    // Splice can only be used when one of the files is a pipe.
    if in_file.downcast_file::<PipeFileObject>().is_none()
        && out_file.downcast_file::<PipeFileObject>().is_none()
    {
        return error!(EINVAL);
    }

    // TODO(fxbug.dev/119324) implement splice().
    not_implemented!("splice");
    error!(ENOSYS)
}
