// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_zircon as zx;
use std::{cell::RefCell, fmt};

use crate::{
    task::CurrentTask,
    types::{pid_t, Errno},
};

macro_rules! log {
    (level = $level:ident, $($arg:tt)*) => {{
        #[cfg(not(feature = "disable_logging"))]
        {
            $crate::logging::with_current_task_info(|_task_info| {
                tracing::$level!(tag = %_task_info, $($arg)*);
            });
        }
    }};
}

macro_rules! log_trace {
    ($($arg:tt)*) => {
        $crate::logging::log!(level = trace, $($arg)*)
    };
}

macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::logging::log!(level = info, $($arg)*)
    };
}

macro_rules! log_warn {
    ($($arg:tt)*) => {
        $crate::logging::log!(level = warn, $($arg)*)
    };
}

macro_rules! log_error {
    ($($arg:tt)*) => {
        $crate::logging::log!(level = error, $($arg)*)
    };
}

macro_rules! not_implemented {
    ($($arg:tt)*) => (
        $crate::logging::log!(level = warn, tag = "not_implemented", $($arg)*)
    )
}

#[allow(unused)]
macro_rules! not_implemented_log_once {
    ($($arg:tt)*) => (
        {
            static DID_LOG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
            if !DID_LOG.swap(true, std::sync::atomic::Ordering::AcqRel) {
                not_implemented!($($arg)*);
            }
        }
    )
}

// Public re-export of macros allows them to be used like regular rust items.
pub(crate) use log;
pub(crate) use log_error;
pub(crate) use log_info;
pub(crate) use log_trace;
pub(crate) use log_warn;
pub(crate) use not_implemented;
#[allow(unused)]
pub(crate) use not_implemented_log_once;

// Call this when you get an error that should "never" happen, i.e. if it does that means the
// kernel was updated to produce some other error after this match was written.
// TODO(tbodt): find a better way to handle this than a panic.
#[track_caller]
pub fn impossible_error(status: zx::Status) -> Errno {
    panic!("encountered impossible error: {status}");
}

fn truncate_name(name: &[u8]) -> std::ffi::CString {
    std::ffi::CString::from_vec_with_nul(
        name.iter()
            .map(|c| if *c == b'\0' { b'?' } else { *c })
            .take(zx::sys::ZX_MAX_NAME_LEN - 1)
            .chain(b"\0".iter().cloned())
            .collect(),
    )
    .expect("all the null bytes should have been replace with an escape")
}

pub fn set_zx_name(obj: &impl zx::AsHandleRef, name: &[u8]) {
    obj.set_name(&truncate_name(name)).map_err(impossible_error).unwrap();
}

/// Set the context for log messages from this thread. Should only be called when a thread has been
/// created to execute a user-level task, and should only be called once at the start of that
/// thread's execution.
pub fn set_current_task_info(current_task: &CurrentTask) {
    CURRENT_TASK_INFO.with(|task_info| {
        *task_info.borrow_mut() = TaskDebugInfo::User {
            pid: current_task.task.thread_group.leader,
            tid: current_task.id,
            command: current_task.task.command().to_string_lossy().to_string(),
        };
    });
}

/// Access this thread's task info for debugging. Intended for use internally by Starnix's log
/// macros.
///
/// *Do not use this for kernel logic.* If you need access to the current pid/tid/etc for the
/// purposes of writing kernel logic beyond logging for debugging purposes, those should be accessed
/// through the `CurrentTask` type as an argument explicitly passed to your function.
pub fn with_current_task_info<T>(f: impl FnOnce(&(dyn fmt::Display)) -> T) -> T {
    CURRENT_TASK_INFO.with(|task_info| f(&task_info.borrow()))
}

/// Used to track the current thread's logical context.
enum TaskDebugInfo {
    /// The thread with this set is used for internal logic within the starnix kernel.
    Kernel,
    /// The thread with this set is used to service syscalls for a specific user thread, and this
    /// describes the user thread's identity.
    User { pid: pid_t, tid: pid_t, command: String },
}

// TODO(b/280356702) replace this with a tracing span
thread_local! {
    /// When a thread in this kernel is started, it is a kthread by default. Once the thread
    /// becomes aware of the user-level task it is executing, this thread-local should be set to
    /// include that info.
    static CURRENT_TASK_INFO: RefCell<TaskDebugInfo> = RefCell::new(TaskDebugInfo::Kernel);
}

impl fmt::Display for TaskDebugInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Kernel => write!(f, "kthread"),
            Self::User { pid, tid, command } => write!(f, "{}:{}[{}]", pid, tid, command),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::testing::*;
    use std::ffi::{CStr, CString};
    use zx::{sys, AsHandleRef};

    #[fuchsia::test]
    async fn test_truncate_name() {
        assert_eq!(truncate_name(b"foo").as_ref(), CStr::from_bytes_with_nul(b"foo\0").unwrap());
        assert_eq!(truncate_name(b"").as_ref(), CStr::from_bytes_with_nul(b"\0").unwrap());
        assert_eq!(
            truncate_name(b"1234567890123456789012345678901234567890").as_ref(),
            CStr::from_bytes_with_nul(b"1234567890123456789012345678901\0").unwrap()
        );
        assert_eq!(truncate_name(b"a\0b").as_ref(), CStr::from_bytes_with_nul(b"a?b\0").unwrap());
    }

    #[fuchsia::test]
    async fn test_long_name() {
        let (_kernel, current_task) = create_kernel_and_task();
        let bytes = [1; sys::ZX_MAX_NAME_LEN];
        let name = CString::new(bytes).unwrap();

        let max_bytes = [1; sys::ZX_MAX_NAME_LEN - 1];
        let expected_name = CString::new(max_bytes).unwrap();

        set_zx_name(&current_task.thread_group.process, name.as_bytes());
        assert_eq!(current_task.thread_group.process.get_name(), Ok(expected_name));
    }

    #[fuchsia::test]
    async fn test_max_length_name() {
        let (_kernel, current_task) = create_kernel_and_task();
        let bytes = [1; sys::ZX_MAX_NAME_LEN - 1];
        let name = CString::new(bytes).unwrap();

        set_zx_name(&current_task.thread_group.process, name.as_bytes());
        assert_eq!(current_task.thread_group.process.get_name(), Ok(name));
    }

    #[fuchsia::test]
    async fn test_short_name() {
        let (_kernel, current_task) = create_kernel_and_task();
        let bytes = [1; sys::ZX_MAX_NAME_LEN - 10];
        let name = CString::new(bytes).unwrap();

        set_zx_name(&current_task.thread_group.process, name.as_bytes());
        assert_eq!(current_task.thread_group.process.get_name(), Ok(name));
    }
}
