// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::{CurrentTask, EventHandler, WaitCanceler, Waiter};
use crate::vfs::buffers::{AncillaryData, InputBuffer, MessageReadInfo, OutputBuffer};
use crate::vfs::file_server::serve_file;
use crate::vfs::socket::{
    Socket, SocketAddress, SocketDomain, SocketHandle, SocketMessageFlags, SocketProtocol,
    SocketType,
};
use crate::vfs::{
    fileops_impl_nonseekable, fileops_impl_noop_sync, FileHandle, FileObject, FileOps,
};
use starnix_sync::{FileOpsCore, LockEqualOrBefore, Locked, Unlocked};
use starnix_syscalls::{SyscallArg, SyscallResult};
use starnix_uapi::error;
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::vfs::FdEvents;
use zx::HandleBased;

pub fn new_socket_file(
    current_task: &CurrentTask,
    domain: SocketDomain,
    socket_type: SocketType,
    open_flags: OpenFlags,
    protocol: SocketProtocol,
) -> Result<FileHandle, Errno> {
    Ok(Socket::new_file(
        current_task,
        Socket::new(current_task, domain, socket_type, protocol)?,
        open_flags,
    ))
}

pub struct SocketFile {
    pub(super) socket: SocketHandle,
}

impl FileOps for SocketFile {
    fileops_impl_nonseekable!();
    fileops_impl_noop_sync!();

    fn read(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
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
        let info =
            self.recvmsg(locked, current_task, file, data, SocketMessageFlags::empty(), None)?;
        Ok(info.bytes_read)
    }

    fn write(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        debug_assert!(offset == 0);
        self.sendmsg(locked, current_task, file, data, None, vec![], SocketMessageFlags::empty())
    }

    fn wait_async(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        waiter: &Waiter,
        events: FdEvents,
        handler: EventHandler,
    ) -> Option<WaitCanceler> {
        Some(self.socket.wait_async(locked, current_task, waiter, events, handler))
    }

    fn query_events(
        &self,
        locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
    ) -> Result<FdEvents, Errno> {
        self.socket.query_events(locked, current_task)
    }

    fn ioctl(
        &self,
        locked: &mut Locked<'_, Unlocked>,
        file: &FileObject,
        current_task: &CurrentTask,
        request: u32,
        arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        self.socket.ioctl(locked, file, current_task, request, arg)
    }

    fn close(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
    ) {
        self.socket.close();
    }

    /// Return a handle that allows access to this file descritor through the zxio protocols.
    ///
    /// If None is returned, the file will act as if it was a fd to `/dev/null`.
    fn to_handle(
        &self,
        file: &FileObject,
        current_task: &CurrentTask,
    ) -> Result<Option<zx::Handle>, Errno> {
        if let Some(handle) = self.socket.to_handle(file, current_task)? {
            Ok(Some(handle))
        } else {
            serve_file(current_task, file).map(|c| Some(c.0.into_handle()))
        }
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
    pub fn sendmsg<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        file: &FileObject,
        data: &mut dyn InputBuffer,
        mut dest_address: Option<SocketAddress>,
        mut ancillary_data: Vec<AncillaryData>,
        flags: SocketMessageFlags,
    ) -> Result<usize, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        debug_assert!(data.bytes_read() == 0);

        // TODO: Implement more `flags`.
        let mut op = |locked: &mut Locked<'_, L>| {
            let offset_before = data.bytes_read();
            let sent_bytes = self.socket.write(
                locked,
                current_task,
                data,
                &mut dest_address,
                &mut ancillary_data,
            )?;
            debug_assert!(data.bytes_read() - offset_before == sent_bytes);
            if data.available() > 0 {
                return error!(EAGAIN);
            }
            Ok(())
        };

        let result = if flags.contains(SocketMessageFlags::DONTWAIT) {
            op(locked)
        } else {
            let deadline = self.socket.send_timeout().map(zx::MonotonicInstant::after);
            file.blocking_op(
                locked,
                current_task,
                FdEvents::POLLOUT | FdEvents::POLLHUP,
                deadline,
                op,
            )
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
    pub fn recvmsg<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        file: &FileObject,
        data: &mut dyn OutputBuffer,
        flags: SocketMessageFlags,
        deadline: Option<zx::MonotonicInstant>,
    ) -> Result<MessageReadInfo, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        // TODO: Implement more `flags`.
        let mut read_info = MessageReadInfo::default();

        let mut op = |locked: &mut Locked<'_, L>| {
            let mut info = self.socket.read(locked, current_task, data, flags)?;
            read_info.append(&mut info);
            read_info.address = info.address;

            let should_wait_all = self.socket.socket_type == SocketType::Stream
                && flags.contains(SocketMessageFlags::WAITALL)
                && !self.socket.query_events(locked, current_task)?.contains(FdEvents::POLLHUP);
            if should_wait_all && data.available() > 0 {
                return error!(EAGAIN);
            }
            Ok(())
        };

        let dont_wait =
            flags.intersects(SocketMessageFlags::DONTWAIT | SocketMessageFlags::ERRQUEUE);
        let result = if dont_wait {
            op(locked)
        } else {
            let deadline =
                deadline.or_else(|| self.socket.receive_timeout().map(zx::MonotonicInstant::after));
            file.blocking_op(
                locked,
                current_task,
                FdEvents::POLLIN | FdEvents::POLLHUP,
                deadline,
                op,
            )
        };

        if read_info.bytes_read == 0 {
            // We can only return an error if no data was actually read. If partial data was
            // read, swallow the error and return how much was read.
            result?;
        }
        Ok(read_info)
    }
}
