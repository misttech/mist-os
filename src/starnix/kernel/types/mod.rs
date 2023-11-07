// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod arc_key;
mod auth;
mod device_type;
mod file_mode;
mod mount_flags;
mod open_flags;
mod personality;
mod resource_limits;
mod seal_flags;
mod stats;
mod time;
mod union;
mod user_address;
mod user_buffer;

pub mod as_any;
pub mod errno;
pub mod kcmp;
pub mod ownership;
pub mod range_ext;
pub mod signals;
pub mod uapi;

pub(crate) use union::*;

pub use arc_key::*;
pub use auth::*;
pub use device_type::*;
pub use errno::*;
pub use file_mode::*;
pub use mount_flags::*;
pub use open_flags::*;
pub use ownership::*;
pub use personality::*;
pub use resource_limits::*;
pub use seal_flags::*;
pub use signals::*;
pub use stats::*;
pub use time::*;
pub use uapi::*;
pub use user_address::*;
pub use user_buffer::*;

// Manually export names that are ambiguous between the name below and the one defined in uapi.
pub use auth::{
    CAP_CHOWN, CAP_DAC_OVERRIDE, CAP_DAC_READ_SEARCH, CAP_FOWNER, CAP_FSETID, CAP_KILL,
    CAP_LINUX_IMMUTABLE, CAP_MAC_OVERRIDE, CAP_MKNOD, CAP_NET_ADMIN, CAP_NET_RAW, CAP_SETGID,
    CAP_SETPCAP, CAP_SETUID, CAP_SYS_ADMIN, CAP_SYS_BOOT, CAP_SYS_CHROOT, CAP_SYS_NICE,
    CAP_SYS_PTRACE, CAP_SYS_RESOURCE, CAP_WAKE_ALARM,
};

pub use errno::{
    EACCES, EAGAIN, EBADF, EEXIST, EINPROGRESS, EINTR, EINVAL, ENAMETOOLONG, ENOENT, ENOSPC,
    ENOSYS, ENOTDIR, EPERM, ETIMEDOUT,
};

pub use signals::{
    SIGABRT, SIGALRM, SIGBUS, SIGCHLD, SIGCONT, SIGFPE, SIGHUP, SIGILL, SIGINT, SIGIO, SIGKILL,
    SIGPIPE, SIGPROF, SIGPWR, SIGQUIT, SIGRTMIN, SIGSEGV, SIGSTKFLT, SIGSTOP, SIGSYS, SIGTERM,
    SIGTRAP, SIGTSTP, SIGTTIN, SIGTTOU, SIGURG, SIGUSR1, SIGUSR2, SIGVTALRM, SIGWINCH, SIGXCPU,
    SIGXFSZ,
};
