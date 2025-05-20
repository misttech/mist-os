// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mm::PAGE_SIZE;
use crate::vfs::inotify::InotifyLimits;
use std::sync::atomic::{AtomicI32, AtomicUsize};

pub struct SocketLimits {
    /// The maximum backlog size for a socket.
    pub max_connections: AtomicI32,
}

pub struct SystemLimits {
    /// Limits applied to inotify objects.
    pub inotify: InotifyLimits,

    /// Limits applied to socket objects.
    pub socket: SocketLimits,

    /// The maximum size of pipes in the system.
    pub pipe_max_size: AtomicUsize,

    /// Whether IoUring is disabled.
    ///
    ///  0 -> io_uring is enabled (default)
    ///  1 -> io_uring is enabled for processes in the io_uring_group
    ///  2 -> io_uring is disabled
    ///
    /// See https://docs.kernel.org/admin-guide/sysctl/kernel.html#io-uring-disabled
    pub io_uring_disabled: AtomicI32,

    /// If io_uring_disabled is 1, then io_uring is enabled only for processes with CAP_SYS_ADMIN
    /// or that are members of this group.
    ///
    /// See https://docs.kernel.org/admin-guide/sysctl/kernel.html#io-uring-group
    pub io_uring_group: AtomicI32,
}

impl Default for SystemLimits {
    fn default() -> SystemLimits {
        SystemLimits {
            inotify: InotifyLimits {
                max_queued_events: AtomicI32::new(16384),
                max_user_instances: AtomicI32::new(128),
                max_user_watches: AtomicI32::new(1048576),
            },
            socket: SocketLimits { max_connections: AtomicI32::new(4096) },
            pipe_max_size: AtomicUsize::new((*PAGE_SIZE * 256) as usize),
            io_uring_disabled: AtomicI32::new(0),
            io_uring_group: AtomicI32::new(-1),
        }
    }
}
