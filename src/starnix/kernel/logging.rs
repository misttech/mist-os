// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::types::{pid_t, Errno};
use fuchsia_zircon as zx;
use std::ffi::CString;

#[cfg(not(feature = "disable_logging"))]
mod enabled {
    use super::*;
    use std::{fmt, sync::Arc};

    #[derive(Clone)]
    pub struct Span(Arc<tracing::Span>);
    pub struct SpanGuard<'a>(tracing::span::Entered<'a>);

    /// Used to track the current thread's logical context.
    /// The thread with this set is used to service syscalls for a specific user thread, and this
    /// describes the user thread's identity.
    struct TaskDebugInfo {
        pid: pid_t,
        tid: pid_t,
        command: String,
    }

    impl fmt::Display for TaskDebugInfo {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "{}:{}[{}]", self.pid, self.tid, self.command)
        }
    }

    impl Span {
        pub fn new(pid: pid_t, tid: pid_t, command: &CString) -> Self {
            let debug_info =
                TaskDebugInfo { pid, tid, command: command.to_string_lossy().to_string() };
            // We wrap this in an Arc, since when we enter the span, the lifetimte of the returned
            // guard is '_ therefore limiting our ability to get exclusive references to the Task
            // within the scope of the Entered guard.
            Self(Arc::new(tracing::info_span!("", tag = %debug_info)))
        }

        pub fn update(&self, pid: pid_t, tid: pid_t, command: &CString) {
            let debug_info =
                TaskDebugInfo { pid, tid, command: command.to_string_lossy().to_string() };
            let debug_info = debug_info.to_string();
            self.0.record("tag", &debug_info.as_str());
        }

        pub fn enter(&self) -> SpanGuard<'_> {
            SpanGuard(self.0.enter())
        }
    }
}

#[cfg(feature = "disable_logging")]
mod disabled {
    use super::*;

    #[derive(Clone)]
    pub struct Span;
    pub struct SpanGuard;

    impl Span {
        pub fn new(pid: pid_t, tid: pid_t, command: &CString) -> Self {
            Span
        }

        pub fn enter(&self) -> SpanGuard {
            SpanGuard
        }

        pub fn update(&self, _pid: pid_t, _tid: pid_t, _command: &CString) {}
    }
}

#[cfg(not(feature = "disable_logging"))]
pub use enabled::*;

#[cfg(feature = "disable_logging")]
pub use disabled::*;

macro_rules! log {
    (level = $level:ident, $($arg:tt)*) => {{
        #[cfg(not(feature = "disable_logging"))]
        tracing::$level!($($arg)*);
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
