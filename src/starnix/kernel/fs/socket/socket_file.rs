// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;

use crate::fs::{buffers::*, socket::*, *};
use crate::syscalls::SyscallResult;
use crate::task::*;
use crate::types::*;

pub struct SocketFile {
    socket: SocketHandle,
}

impl FileOps for SocketFile {
    fileops_impl_nonseekable!();

    fn read(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        // The behavior of recv differs from read: recv will block if given a zero-size buffer when
        // there's no data available, but read will immediately return 0.
        if data.available() == 0 {
            return Ok(0);
        }
        let info = self.recvmsg(current_task, file, data, SocketMessageFlags::empty(), None)?;
        Ok(info.bytes_read)
    }

    fn write(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        self.sendmsg(current_task, file, data, None, vec![], SocketMessageFlags::empty())
    }

    fn wait_async(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(self.socket.wait_async(current_task, waiter, events, handler))
    }

    fn query_events(&self, current_task: &CurrentTask) -> FdEvents {
        self.socket.query_events(current_task)
    }

    fn ioctl(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        user_addr: UserAddress,
    ) -> Result<SyscallResult, Errno> {
        self.socket.ioctl(current_task, request, user_addr)
    }

    fn close(&self, _file: &FileObject) {
        self.socket.close();
    }
}

impl SocketFile {
    pub fn new(socket: SocketHandle) -> Box<Self> {
        Box::new(SocketFile { socket })
    }

    /// Writes the provided data into the socket in this file.
    ///
    /// The provided control message is
    ///
    /// # Parameters
    /// - `task`: The task that the user buffers belong to.
    /// - `file`: The file that will be used for the `blocking_op`.
    /// - `data`: The user buffers to read data from.
    /// - `control_bytes`: Control message bytes to write to the socket.
    pub fn sendmsg(
        &self,
        current_task: &CurrentTask,
        file: &FileObject,
        data: &mut dyn InputBuffer,
        mut dest_address: Option<SocketAddress>,
        mut ancillary_data: Vec<AncillaryData>,
        flags: SocketMessageFlags,
    ) -> Result<usize, Errno> {
        debug_assert!(data.bytes_read() == 0);

        // TODO: Implement more `flags`.
        let mut op = || {
            let offset_before = data.bytes_read();
            let sent_bytes =
                self.socket.write(current_task, data, &mut dest_address, &mut ancillary_data)?;
            debug_assert!(data.bytes_read() - offset_before == sent_bytes);
            if sent_bytes > 0 {
                return error!(EAGAIN);
            }
            Ok(())
        };

        let result = if flags.contains(SocketMessageFlags::DONTWAIT) {
            op()
        } else {
            let deadline = self.socket.send_timeout().map(zx::Time::after);
            file.blocking_op(current_task, FdEvents::POLLOUT | FdEvents::POLLHUP, deadline, op)
        };

        let bytes_written = data.bytes_read();
        if bytes_written == 0 {
            // We can only return an error if no data was actually sent. If partial data was
            // sent, swallow the error and return how much was sent.
            result?;
        }
        Ok(bytes_written)
    }

    /// Reads data from the socket in this file into `data`.
    ///
    /// # Parameters
    /// - `file`: The file that will be used to wait if necessary.
    /// - `task`: The task that the user buffers belong to.
    /// - `data`: The user buffers to write to.
    ///
    /// Returns the number of bytes read, as well as any control message that was encountered.
    pub fn recvmsg(
        &self,
        current_task: &CurrentTask,
        file: &FileObject,
        data: &mut dyn OutputBuffer,
        flags: SocketMessageFlags,
        deadline: Option<zx::Time>,
    ) -> Result<MessageReadInfo, Errno> {
        // TODO: Implement more `flags`.
        let mut read_info = MessageReadInfo::default();

        let mut op = || {
            let mut info = self.socket.read(current_task, data, flags)?;
            read_info.append(&mut info);
            read_info.address = info.address;

            let should_wait_all = self.socket.socket_type == SocketType::Stream
                && flags.contains(SocketMessageFlags::WAITALL)
                && !self.socket.query_events(current_task).contains(FdEvents::POLLHUP);
            if should_wait_all && data.available() > 0 {
                return error!(EAGAIN);
            }
            Ok(())
        };

        let result = if flags.contains(SocketMessageFlags::DONTWAIT) {
            op()
        } else {
            let deadline = deadline.or_else(|| self.socket.receive_timeout().map(zx::Time::after));
            file.blocking_op(current_task, FdEvents::POLLIN | FdEvents::POLLHUP, deadline, op)
        };

        if read_info.bytes_read == 0 {
            // We can only return an error if no data was actually read. If partial data was
            // read, swallow the error and return how much was read.
            result?;
        }
        Ok(read_info)
    }
}
