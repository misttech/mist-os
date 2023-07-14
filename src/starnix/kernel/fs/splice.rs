// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fs::{
        buffers::{VecInputBuffer, VecOutputBuffer},
        pipe::PipeFileObject,
        FdNumber, FileHandle,
    },
    lock::{ordered_lock, MutexGuard},
    logging::{log_warn, not_implemented},
    mm::{MemoryAccessorExt, PAGE_SIZE},
    task::CurrentTask,
    types::{error, off_t, uapi, Errno, OpenFlags, UserAddress, UserRef, MAX_RW_COUNT},
};
use std::{cmp::Ordering, sync::Arc, usize};

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

    // Lock the in_file offset for the entire operation.
    let mut in_offset = in_file.offset.lock();
    let mut total_written = 0;

    match (|| -> Result<(), Errno> {
        while count > 0 {
            let limit = std::cmp::min(*PAGE_SIZE as usize, count);
            let mut buffer = VecOutputBuffer::new(limit);
            let read = in_file.read_at(current_task, *in_offset as usize, &mut buffer)?;
            let mut buffer = Vec::from(buffer);
            buffer.truncate(read);
            let written = out_file.write(current_task, &mut VecInputBuffer::from(buffer))?;
            *in_offset += written as i64;
            total_written += written;
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
struct SplicedFile {
    file: FileHandle,
    offset_ref: UserRef<off_t>,
    offset: Option<usize>,
}

impl SplicedFile {
    fn new(
        current_task: &CurrentTask,
        fd: FdNumber,
        offset_ref: UserRef<off_t>,
    ) -> Result<Self, Errno> {
        let file = current_task.files.get(fd)?;
        let offset = if offset_ref.is_null() {
            None
        } else {
            if !file.is_seekable() {
                return error!(ESPIPE);
            }
            let offset = current_task.read_object(offset_ref)?;
            if offset < 0 {
                return error!(EINVAL);
            } else {
                Some(offset as usize)
            }
        };
        Ok(Self { file, offset_ref, offset })
    }

    fn maybe_as_pipe(&self) -> Option<&PipeFileObject> {
        self.file.downcast_file::<PipeFileObject>()
    }

    /// Returns the effective offset at which to execute read/write
    /// - Return None if the file has no persistent offsets, and all read/write must happen at 0.
    /// - Returns Some(self.offset) if the offset has been specified by the user
    /// - Returns Some(*file_offset) otherwise
    fn effective_offset(&self, file_offset: &MutexGuard<'_, off_t>) -> Option<usize> {
        self.file.has_persistent_offsets().then(|| self.offset.unwrap_or(**file_offset as usize))
    }

    fn update_offset(
        &self,
        current_task: &CurrentTask,
        offset_guard: &mut MutexGuard<'_, off_t>,
        spliced: usize,
    ) -> Result<(), Errno> {
        match &self.offset {
            None if self.file.has_persistent_offsets() => {
                // The file has persistent offsets, and the user didn't specify an offset. The
                // internal file offset must be updated.
                **offset_guard += spliced as off_t;
                Ok(())
            }
            Some(v) => {
                // The file is seekable and the user specified an offset. The new offset must be
                // written back to userspace.
                current_task
                    .mm
                    .write_object(self.offset_ref, &((*v + spliced) as off_t))
                    .map(|_| ())
            }
            // Nothing to be done.
            _ => Ok(()),
        }
    }
}

pub fn splice(
    current_task: &CurrentTask,
    fd_in: FdNumber,
    off_in: UserRef<off_t>,
    fd_out: FdNumber,
    off_out: UserRef<off_t>,
    len: usize,
    flags: u32,
) -> Result<usize, Errno> {
    const KNOWN_FLAGS: u32 =
        uapi::SPLICE_F_MOVE | uapi::SPLICE_F_NONBLOCK | uapi::SPLICE_F_MORE | uapi::SPLICE_F_GIFT;
    if flags & !KNOWN_FLAGS != 0 {
        log_warn!("Unexpected flag for splice: {:#x}", flags & !KNOWN_FLAGS);
        return error!(EINVAL);
    }

    let non_blocking = flags & uapi::SPLICE_F_NONBLOCK != 0;

    let file_in = SplicedFile::new(current_task, fd_in, off_in)?;
    let file_out = SplicedFile::new(current_task, fd_out, off_out)?;

    // out_fd has the O_APPEND flag set. This is not supported by splice().
    if file_out.file.flags().contains(OpenFlags::APPEND) {
        return error!(EINVAL);
    }

    // The 2 fds cannot refer to the same pipe.
    let node_cmp = Arc::as_ptr(&file_in.file.name.entry.node)
        .cmp(&Arc::as_ptr(&file_out.file.name.entry.node));
    if node_cmp == Ordering::Equal {
        return error!(EINVAL);
    }

    // Lock offsets.
    let (mut file_in_offset_guard, mut file_out_offset_guard) =
        ordered_lock(&file_in.file.offset, &file_out.file.offset);

    let spliced = match (file_in.maybe_as_pipe(), file_out.maybe_as_pipe()) {
        // Splice can only be used when one of the files is a pipe.
        (None, None) => error!(EINVAL),
        (pipe_in, Some(pipe_out)) if pipe_in.is_none() || node_cmp == Ordering::Less => pipe_out
            .splice_from(
                current_task,
                &file_out.file,
                &file_in.file,
                file_in.effective_offset(&file_in_offset_guard),
                len,
                non_blocking,
            ),
        (Some(pipe_in), _) => pipe_in.splice_to(
            current_task,
            &file_in.file,
            &file_out.file,
            file_out.effective_offset(&file_out_offset_guard),
            len,
            non_blocking,
        ),
        _ => unreachable!(),
    }?;

    // Update both file offset before returning in case of error writing back to
    // userspace.
    let update_offset_in = file_in.update_offset(current_task, &mut file_in_offset_guard, spliced);
    file_out.update_offset(current_task, &mut file_out_offset_guard, spliced)?;
    update_offset_in?;
    Ok(spliced)
}
