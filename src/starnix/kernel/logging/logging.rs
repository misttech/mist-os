// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_uapi::errors::Errno;
use starnix_uapi::pid_t;
use std::cell::RefCell;
use std::ffi::CString;
use std::fmt;

// This needs to be available to the macros in this module without clients having to depend on
// tracing themselves.
#[doc(hidden)]
pub use tracing as __tracing;

pub use tracing::Level;

/// Used to track the current thread's logical context.
enum TaskDebugInfo {
    /// The thread with this set is used for internal logic within the starnix kernel.
    Kernel,
    /// The thread with this set is used to service syscalls for a specific user thread, and this
    /// describes the user thread's identity.
    User { pid: pid_t, tid: pid_t, command: String },
    /// Unknown info. This happens when trying to log while in the destructor of a thread local
    /// variable.
    Unknown,
}

thread_local! {
    /// When a thread in this kernel is started, it is a kthread by default. Once the thread
    /// becomes aware of the user-level task it is executing, this thread-local should be set to
    /// include that info.
    static CURRENT_TASK_INFO: RefCell<TaskDebugInfo> = const { RefCell::new(TaskDebugInfo::Kernel) } ;
}

impl fmt::Display for TaskDebugInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kernel => write!(f, "kthread"),
            Self::User { pid, tid, command } => write!(f, "{}:{}[{}]", pid, tid, command),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

#[inline]
pub const fn logs_enabled() -> bool {
    !cfg!(feature = "disable_logging")
}

#[inline]
pub const fn trace_debug_logs_enabled() -> bool {
    // Allow trace and debug logs if we are in a debug (non-release) build
    // or feature `trace_and_debug_logs_in_release` is enabled.
    logs_enabled() && (cfg!(debug_assertions) || cfg!(feature = "trace_and_debug_logs_in_release"))
}

#[macro_export]
macro_rules! log_trace {
    ($($arg:tt)*) => {
        if $crate::trace_debug_logs_enabled() {
            $crate::with_current_task_info(|_task_info| {
                $crate::__tracing::trace!(tag = %_task_info, $($arg)*);
            });
        }
    };
}

#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        if $crate::trace_debug_logs_enabled() {
            $crate::with_current_task_info(|_task_info| {
                $crate::__tracing::debug!(tag = %_task_info, $($arg)*);
            });
        }
    };
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        if $crate::logs_enabled() {
            $crate::with_current_task_info(|_task_info| {
                $crate::__tracing::info!(tag = %_task_info, $($arg)*);
            });
        }
    };
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        if $crate::logs_enabled() {
            $crate::with_current_task_info(|_task_info| {
                $crate::__tracing::warn!(tag = %_task_info, $($arg)*);
            });
        }
    };
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        if $crate::logs_enabled() {
            $crate::with_current_task_info(|_task_info| {
                $crate::__tracing::error!(tag = %_task_info, $($arg)*);
            });
        }
    };
}

// Note that we can't just call `event!` with a non-const level since
// tracing requires the metadata fields to be static.
// See: https://github.com/tokio-rs/tracing/issues/2730
#[macro_export]
macro_rules! log {
    ($lvl:expr, $($arg:tt)*) => {
         match $lvl {
             $crate::Level::TRACE => $crate::log_trace!($($arg)*),
             $crate::Level::DEBUG => $crate::log_debug!($($arg)*),
             $crate::Level::INFO => $crate::log_info!($($arg)*),
             $crate::Level::WARN => $crate::log_warn!($($arg)*),
             $crate::Level::ERROR => $crate::log_error!($($arg)*),
         }
    };
}

// Call this when you get an error that should "never" happen, i.e. if it does that means the
// kernel was updated to produce some other error after this match was written.
#[track_caller]
pub fn impossible_error(status: zx::Status) -> Errno {
    panic!("encountered impossible error: {status}");
}

pub fn set_zx_name(obj: &impl zx::AsHandleRef, name: impl AsRef<[u8]>) {
    obj.set_name(&zx::Name::from_bytes_lossy(name.as_ref())).map_err(impossible_error).unwrap();
}

pub fn with_zx_name<O: zx::AsHandleRef>(obj: O, name: impl AsRef<[u8]>) -> O {
    set_zx_name(&obj, name);
    obj
}

/// Set the context for log messages from this thread. Should only be called when a thread has been
/// created to execute a user-level task, and should only be called once at the start of that
/// thread's execution.
pub fn set_current_task_info(name: &CString, pid: pid_t, tid: pid_t) {
    set_zx_name(&fuchsia_runtime::thread_self(), name.as_bytes());
    CURRENT_TASK_INFO.with(|task_info| {
        *task_info.borrow_mut() =
            TaskDebugInfo::User { pid, tid, command: name.to_string_lossy().to_string() };
    });
}

/// Access this thread's task info for debugging. Intended for use internally by Starnix's log
/// macros.
///
/// *Do not use this for kernel logic.* If you need access to the current pid/tid/etc for the
/// purposes of writing kernel logic beyond logging for debugging purposes, those should be accessed
/// through the `CurrentTask` type as an argument explicitly passed to your function.
#[doc(hidden)]
pub fn with_current_task_info<T>(f: impl Fn(&(dyn fmt::Display)) -> T) -> T {
    match CURRENT_TASK_INFO.try_with(|task_info| f(&task_info.borrow())) {
        Ok(value) => value,
        Err(_) => f(&TaskDebugInfo::Unknown),
    }
}
