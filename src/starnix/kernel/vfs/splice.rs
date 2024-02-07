// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    mm::{MemoryAccessorExt, PAGE_SIZE},
    task::CurrentTask,
    vfs::{
        buffers::{
            UserBuffersInputBuffer, UserBuffersOutputBuffer, VecInputBuffer, VecOutputBuffer,
        },
        pipe::{Pipe, PipeFileObject, PipeOperands},
        FdNumber, FileHandle,
    },
};
use starnix_logging::track_stub;
use starnix_sync::{LockBefore, Locked, ReadOps, Unlocked, WriteOps};
use starnix_uapi::{
    errno, error,
    errors::Errno,
    off_t,
    open_flags::OpenFlags,
    uapi,
    user_address::{UserAddress, UserRef},
    user_buffer::MAX_RW_COUNT,
};

pub fn sendfile<L>(
    locked: &mut Locked<'_, L>,
    current_task: &CurrentTask,
    out_fd: FdNumber,
    in_fd: FdNumber,
    user_offset: UserRef<off_t>,
    count: i32,
) -> Result<usize, Errno>
where
    L: LockBefore<ReadOps>,
    L: LockBefore<WriteOps>,
{
    let out_file = current_task.files.get(out_fd)?;
    let in_file = current_task.files.get(in_fd)?;

    let maybe_offset =
        if user_offset.is_null() { None } else { Some(current_task.read_object(user_offset)?) };

    if count < 0 {
        return error!(EINVAL);
    }

    if !in_file.flags().can_read() || !out_file.flags().can_write() {
        return error!(EBADF);
    }

    // We need the in file to be seekable because we use read_at below, but this is also a proxy for
    // checking that the file supports mmap-like operations.
    if !in_file.is_seekable() {
        return error!(EINVAL);
    }

    // out_fd has the O_APPEND flag set.  This is not currently supported by sendfile().
    // See https://man7.org/linux/man-pages/man2/sendfile.2.html#ERRORS
    if out_file.flags().contains(OpenFlags::APPEND) {
        return error!(EINVAL);
    }

    let count = count as usize;
    let mut count = std::cmp::min(count, *MAX_RW_COUNT);

    let (mut offset, mut update_offset): (usize, Box<dyn FnMut(off_t) -> Result<(), Errno>>) =
        if let Some(offset) = maybe_offset {
            (
                offset.try_into().map_err(|_| errno!(EINVAL))?,
                Box::new(|updated_offset| -> Result<(), Errno> {
                    current_task.write_object(user_offset, &updated_offset)?;
                    Ok(())
                }),
            )
        } else {
            // Lock the in_file offset for the entire operation.
            let mut in_offset = in_file.offset.lock();
            let offset = *in_offset;
            (
                offset as usize,
                Box::new(move |updated_offset| -> Result<(), Errno> {
                    *in_offset = updated_offset;
                    Ok(())
                }),
            )
        };
    let mut total_written = 0;

    match (|| -> Result<(), Errno> {
        while count > 0 {
            let limit = std::cmp::min(*PAGE_SIZE as usize, count);
            let mut buffer = VecOutputBuffer::new(limit);
            let read = { in_file.read_at(locked, current_task, offset, &mut buffer)? };
            let mut buffer = Vec::from(buffer);
            buffer.truncate(read);
            let written =
                out_file.write(locked, current_task, &mut VecInputBuffer::from(buffer))?;
            offset += written;
            total_written += written;
            update_offset(offset as off_t)?;
            if read < limit || written < read {
                break;
            }
            count -= written;
        }
        Ok(())
    })() {
        Ok(()) => Ok(total_written),
        Err(e) => {
            if total_written > 0 {
                Ok(total_written)
            } else {
                match e.code.error_code() {
                    uapi::EISDIR => error!(EINVAL),
                    _ => Err(e),
                }
            }
        }
    }
}

#[derive(Debug)]
struct SplicedOperand {
    file: FileHandle,
    user_offset: UserRef<off_t>,
    maybe_offset: Option<usize>,
}

impl SplicedOperand {
    fn new(
        current_task: &CurrentTask,
        fd: FdNumber,
        user_offset: UserRef<off_t>,
    ) -> Result<Self, Errno> {
        let file = current_task.files.get(fd)?;
        let maybe_offset = if user_offset.is_null() {
            None
        } else {
            if !file.is_seekable() {
                return error!(ESPIPE);
            }
            let offset = current_task.read_object(user_offset)?;
            if offset < 0 {
                return error!(EINVAL);
            } else {
                Some(offset as usize)
            }
        };
        Ok(Self { file, user_offset, maybe_offset })
    }

    fn maybe_as_pipe(&self) -> Option<&PipeFileObject> {
        self.file.downcast_file::<PipeFileObject>()
    }

    fn maybe_write_result_offset(
        &self,
        current_task: &CurrentTask,
        spliced: usize,
    ) -> Result<(), Errno> {
        if let Some(offset) = self.maybe_offset {
            let new_offset = (offset + spliced) as off_t;
            current_task.write_object(self.user_offset, &new_offset)?;
        }
        Ok(())
    }
}

pub fn splice<L>(
    locked: &mut Locked<'_, L>,
    current_task: &CurrentTask,
    fd_in: FdNumber,
    off_in: UserRef<off_t>,
    fd_out: FdNumber,
    off_out: UserRef<off_t>,
    len: usize,
    flags: u32,
) -> Result<usize, Errno>
where
    L: LockBefore<ReadOps>,
    L: LockBefore<WriteOps>,
{
    const KNOWN_FLAGS: u32 =
        uapi::SPLICE_F_MOVE | uapi::SPLICE_F_NONBLOCK | uapi::SPLICE_F_MORE | uapi::SPLICE_F_GIFT;
    if flags & !KNOWN_FLAGS != 0 {
        track_stub!(TODO("https://fxbug.dev/322875389"), "splice flags", flags & !KNOWN_FLAGS);
        return error!(EINVAL);
    }

    let non_blocking = flags & uapi::SPLICE_F_NONBLOCK != 0;

    let operand_in = SplicedOperand::new(current_task, fd_in, off_in)?;
    let operand_out = SplicedOperand::new(current_task, fd_out, off_out)?;

    // out_fd has the O_APPEND flag set. This is not supported by splice().
    if operand_out.file.flags().contains(OpenFlags::APPEND) {
        return error!(EINVAL);
    }

    let spliced = match (operand_in.maybe_as_pipe(), operand_out.maybe_as_pipe()) {
        // Splice can only be used when one of the files is a pipe.
        (None, None) => return error!(EINVAL),
        // If both ends are pipes, use the symmetric `Pipe::splice` function.
        (Some(_), Some(_)) => {
            let PipeOperands { mut read, mut write } = PipeFileObject::lock_pipes(
                current_task,
                &operand_in.file,
                &operand_out.file,
                len,
                non_blocking,
            )?;
            Pipe::splice(&mut read, &mut write, len)?
        }
        (None, Some(pipe_out)) => {
            let spliced = pipe_out.splice_from(
                locked,
                current_task,
                &operand_out.file,
                &operand_in.file,
                operand_in.maybe_offset,
                len,
                non_blocking,
            )?;
            operand_in.maybe_write_result_offset(current_task, spliced)?;
            spliced
        }
        (Some(pipe_in), None) => {
            let spliced = pipe_in.splice_to(
                locked,
                current_task,
                &operand_in.file,
                &operand_out.file,
                operand_out.maybe_offset,
                len,
                non_blocking,
            )?;
            operand_out.maybe_write_result_offset(current_task, spliced)?;
            spliced
        }
    };

    Ok(spliced)
}

pub fn vmsplice(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    fd: FdNumber,
    iovec_addr: UserAddress,
    iovec_count: i32,
    flags: u32,
) -> Result<usize, Errno> {
    const KNOWN_FLAGS: u32 =
        uapi::SPLICE_F_MOVE | uapi::SPLICE_F_NONBLOCK | uapi::SPLICE_F_MORE | uapi::SPLICE_F_GIFT;
    if flags & !KNOWN_FLAGS != 0 {
        track_stub!(TODO("https://fxbug.dev/322875487"), "vmsplice flags", flags & !KNOWN_FLAGS);
        return error!(EINVAL);
    }

    let non_blocking = flags & uapi::SPLICE_F_NONBLOCK != 0;

    let file = current_task.files.get(fd)?;
    let should_write = file.can_write();
    let should_read = file.can_read();

    let iovec = current_task.read_iovec(iovec_addr, iovec_count)?;
    let pipe = file.downcast_file::<PipeFileObject>().ok_or_else(|| errno!(EBADF))?;
    let mut bytes_transferred = 0;

    if should_write {
        let mut in_data = UserBuffersInputBuffer::unified_new(current_task, iovec.clone())?;
        bytes_transferred += pipe.vmsplice_from(current_task, &file, &mut in_data, non_blocking)?;
    }

    if should_read {
        let mut out_data = UserBuffersOutputBuffer::unified_new(current_task, iovec)?;
        bytes_transferred += pipe.vmsplice_to(current_task, &file, &mut out_data, non_blocking)?;
    }

    Ok(bytes_transferred)
}

pub fn tee(
    current_task: &CurrentTask,
    fd_in: FdNumber,
    fd_out: FdNumber,
    len: usize,
    flags: u32,
) -> Result<usize, Errno> {
    const KNOWN_FLAGS: u32 =
        uapi::SPLICE_F_MOVE | uapi::SPLICE_F_NONBLOCK | uapi::SPLICE_F_MORE | uapi::SPLICE_F_GIFT;
    if flags & !KNOWN_FLAGS != 0 {
        track_stub!(TODO("https://fxbug.dev/322874902"), "tee flags", flags & !KNOWN_FLAGS);
        return error!(EINVAL);
    }

    let non_blocking = flags & uapi::SPLICE_F_NONBLOCK != 0;

    let file_in = current_task.files.get(fd_in)?;
    let file_out = current_task.files.get(fd_out)?;

    // tee requires that both files are pipes.
    let PipeOperands { mut read, mut write } =
        PipeFileObject::lock_pipes(current_task, &file_in, &file_out, len, non_blocking)?;
    Pipe::tee(&mut read, &mut write, len)
}
