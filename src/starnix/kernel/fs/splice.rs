// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::{
        fs::{
            buffers::{InputBuffer, InputBufferCallback},
            pipe::PipeFileObject,
            FdNumber, FileHandle, SeekOrigin,
        },
        logging::{log_warn, not_implemented},
        mm::{MemoryAccessorExt, ProtectionFlags, PAGE_SIZE},
        task::CurrentTask,
        types::{errno, error, off_t, uapi, Errno, OpenFlags, UserAddress, UserRef, MAX_RW_COUNT},
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

struct SplicedFile {
    file: FileHandle,
    is_pipe: bool,
}

impl SplicedFile {
    fn new(
        current_task: &CurrentTask,
        fd: FdNumber,
        offset_ref: UserRef<off_t>,
    ) -> Result<Self, Errno> {
        let file = current_task.files.get(fd)?;
        let seekable = file.seek(current_task, 0, SeekOrigin::Cur).is_ok();
        let _offset = if offset_ref.is_null() {
            None
        } else {
            if !seekable {
                return error!(ESPIPE);
            }
            let offset = current_task.mm.read_object(offset_ref)?;
            if offset < 0 {
                return error!(EINVAL);
            } else {
                Some(offset as usize)
            }
        };
        let is_pipe = file.downcast_file::<PipeFileObject>().is_some();
        Ok(Self { file, is_pipe })
    }
}

pub fn splice(
    current_task: &CurrentTask,
    fd_in: FdNumber,
    off_in: UserRef<off_t>,
    fd_out: FdNumber,
    off_out: UserRef<off_t>,
    _len: usize,
    flags: u32,
) -> Result<usize, Errno> {
    const KNOWN_FLAGS: u32 =
        uapi::SPLICE_F_MOVE | uapi::SPLICE_F_NONBLOCK | uapi::SPLICE_F_MORE | uapi::SPLICE_F_GIFT;
    if flags & !KNOWN_FLAGS != 0 {
        log_warn!("Unexpected flag for splice: {:#x}", flags & !KNOWN_FLAGS);
        return error!(EINVAL);
    }

    let file_in = SplicedFile::new(current_task, fd_in, off_in)?;
    let file_out = SplicedFile::new(current_task, fd_out, off_out)?;

    // out_fd has the O_APPEND flag set. This is not supported by splice().
    if file_out.file.flags().contains(OpenFlags::APPEND) {
        return error!(EINVAL);
    }

    // Splice can only be used when one of the files is a pipe.
    if !file_in.is_pipe && !file_out.is_pipe {
        return error!(EINVAL);
    }

    // The 2 fds cannot refer to the same pipe.
    if Arc::ptr_eq(&file_in.file.name.entry.node, &file_out.file.name.entry.node) {
        return error!(EINVAL);
    }

    // TODO(fxbug.dev/119324) implement splice().
    not_implemented!("splice");
    error!(ENOSYS)
}
