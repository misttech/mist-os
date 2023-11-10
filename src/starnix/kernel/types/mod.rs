// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod arc_key;
mod auth;
mod file_mode;
mod mount_flags;
mod open_flags;
mod personality;
mod resource_limits;
mod seal_flags;
mod stats;
mod union;
mod user_address;
mod user_buffer;

pub mod as_any;
pub mod device_type;
pub mod errno;
pub mod kcmp;
pub mod ownership;
pub mod range_ext;
pub mod signals;
pub mod time;
pub mod uapi;

pub(crate) use union::struct_with_union_into_bytes;

pub use arc_key::{ArcKey, PtrKey, WeakKey};
pub use device_type::{
    DeviceType, DYN_MAJOR, INPUT_MAJOR, KEYBOARD_INPUT_MINOR, LOOP_MAJOR, TOUCH_INPUT_MINOR,
    TTY_ALT_MAJOR,
};
pub(crate) use file_mode::mode;
pub use file_mode::{Access, FileMode};
pub use mount_flags::MountFlags;
pub use open_flags::OpenFlags;
pub(crate) use ownership::{async_release_after, release_after, release_on_error};
pub use ownership::{
    debug_assert_no_local_temp_ref, OwnedRef, OwnedRefByRef, Releasable, ReleasableByRef,
    ReleaseGuard, TempRef, TempRefKey, WeakRef,
};
pub use personality::PersonalityFlags;
pub use resource_limits::{Resource, ResourceLimits};
pub use seal_flags::SealFlags;
pub use stats::TaskTimeStats;
pub use uapi::*;
pub use user_address::*;
pub use user_buffer::*;

// Manually export names that are ambiguous between the name below and the one defined in uapi.
pub use auth::{
    Capabilities, PtraceAccessMode, CAP_CHOWN, CAP_DAC_OVERRIDE, CAP_DAC_READ_SEARCH, CAP_FOWNER,
    CAP_FSETID, CAP_KILL, CAP_LINUX_IMMUTABLE, CAP_MAC_OVERRIDE, CAP_MKNOD, CAP_NET_ADMIN,
    CAP_NET_RAW, CAP_SETGID, CAP_SETPCAP, CAP_SETUID, CAP_SYS_ADMIN, CAP_SYS_BOOT, CAP_SYS_CHROOT,
    CAP_SYS_NICE, CAP_SYS_PTRACE, CAP_SYS_RESOURCE, CAP_WAKE_ALARM, PTRACE_MODE_ATTACH_REALCREDS,
    PTRACE_MODE_FSCREDS, PTRACE_MODE_REALCREDS,
};

pub(crate) use crate::types::errno::{
    errno, errno_from_code, errno_from_zxio_code, error, from_status_like_fdio,
};
pub use errno::{
    Errno, ErrnoCode, ErrnoResultExt, SourceContext, EACCES, EAGAIN, EBADF, EEXIST, EINPROGRESS,
    EINTR, ENAMETOOLONG, ENOENT, ENOSPC, ENOSYS, ENOTDIR, EPERM, ERESTARTNOHAND, ERESTARTNOINTR,
    ERESTARTSYS, ERESTART_RESTARTBLOCK, ETIMEDOUT,
};

pub use signals::{
    SigSet, Signal, UncheckedSignal, SIGABRT, SIGALRM, SIGBUS, SIGCHLD, SIGCONT, SIGFPE, SIGHUP,
    SIGILL, SIGINT, SIGIO, SIGKILL, SIGPIPE, SIGPROF, SIGPWR, SIGQUIT, SIGSEGV, SIGSTKFLT, SIGSTOP,
    SIGSYS, SIGTERM, SIGTRAP, SIGTSTP, SIGTTIN, SIGTTOU, SIGURG, SIGUSR1, SIGUSR2, SIGVTALRM,
    SIGWINCH, SIGXCPU, SIGXFSZ, UNBLOCKABLE_SIGNALS,
};
