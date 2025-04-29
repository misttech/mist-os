// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::sync::atomic::{AtomicU64, Ordering};

use linux_uapi::arch32::{
    PERF_EVENT_IOC_DISABLE, PERF_EVENT_IOC_ENABLE, PERF_EVENT_IOC_ID,
    PERF_EVENT_IOC_MODIFY_ATTRIBUTES, PERF_EVENT_IOC_PAUSE_OUTPUT, PERF_EVENT_IOC_PERIOD,
    PERF_EVENT_IOC_QUERY_BPF, PERF_EVENT_IOC_REFRESH, PERF_EVENT_IOC_RESET, PERF_EVENT_IOC_SET_BPF,
    PERF_EVENT_IOC_SET_FILTER, PERF_EVENT_IOC_SET_OUTPUT,
};
use linux_uapi::{
    perf_event_attr, perf_event_read_format_PERF_FORMAT_GROUP,
    perf_event_read_format_PERF_FORMAT_ID, perf_event_read_format_PERF_FORMAT_LOST,
    perf_event_read_format_PERF_FORMAT_TOTAL_TIME_ENABLED,
    perf_event_read_format_PERF_FORMAT_TOTAL_TIME_RUNNING, pid_t,
};
use starnix_logging::track_stub;
use starnix_sync::{FileOpsCore, Locked, RwLock, Unlocked};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::user_address::UserRef;
use starnix_uapi::{error, uapi};

static READ_FORMAT_ID_GENERATOR: AtomicU64 = AtomicU64::new(0);

uapi::check_arch_independent_layout! {
    perf_event_attr {
        type_, // "type" is a reserved keyword so add a trailing underscore.
        size,
        config,
        __bindgen_anon_1,
        sample_type,
        read_format,
        _bitfield_align_1,
        _bitfield_1,
        __bindgen_anon_2,
        bp_type,
        __bindgen_anon_3,
        __bindgen_anon_4,
        branch_sample_type,
        sample_regs_user,
        sample_stack_user,
        clockid,
        sample_regs_intr,
        aux_watermark,
        sample_max_stack,
        __reserved_2,
        aux_sample_size,
        __reserved_3,
        sig_data,
        config3,
    }
}

#[derive(Default)]
struct PerfEventFileState {
    attr: perf_event_attr,
    rf_value: u64, // "count" for the config we passed in for the event.
    // The most recent timestamp (ns) where we changed into an enabled state
    // i.e. the most recent time we got an ENABLE ioctl().
    most_recent_enabled_time: u64,
    // Sum of all previous enablement segment durations (ns). If we are
    // currently in an enabled state, explicitly does NOT include the current
    // segment.
    total_time_running: u64,
    rf_id: u64,
    _rf_lost: u64,
    disabled: u64,
}

struct PerfEventFile {
    _pid: pid_t,
    _cpu: i32,
    perf_event_file: RwLock<PerfEventFileState>,
}

// PerfEventFile object that implements FileOps.
// See https://man7.org/linux/man-pages/man2/perf_event_open.2.html for
// implementation details.
// This object can be saved as a FileDescriptor.
impl FileOps for PerfEventFile {
    // Don't need to implement seek or sync for PerfEventFile.
    fileops_impl_nonseekable!();
    fileops_impl_noop_sync!();

    // See "Reading results" section of https://man7.org/linux/man-pages/man2/perf_event_open.2.html.
    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        // Create/calculate and return the ReadFormatData object.
        // If we create it earlier we might want to change it and it's immutable once created.
        let read_format_data = {
            // Once we get the `value` or count from kernel, we can change this to a read()
            // call instead of write().
            let mut perf_event_file = self.perf_event_file.write();
            let mut total_time_running_including_curr = perf_event_file.total_time_running;

            // Only update values if enabled (either by perf_event_attr or ioctl ENABLE call).
            if perf_event_file.disabled == 0 {
                // Calculate the value or "count" of the config we're interested in.
                // This value should reflect the value we are counting (defined in the config).
                // E.g. for PERF_COUNT_SW_CPU_CLOCK it would return the value from the CPU clock.
                // For now we just return rf_value + 1.
                track_stub!(
                    TODO("https://fxbug.dev/402938671"),
                    "[perf_event_open] implement read_format value"
                );
                perf_event_file.rf_value += 1;

                // Update time duration.
                let curr_time = zx::MonotonicInstant::get().into_nanos() as u64;
                total_time_running_including_curr +=
                    curr_time - perf_event_file.most_recent_enabled_time;
            }

            let mut output = Vec::<u8>::new();
            let value = perf_event_file.rf_value.to_ne_bytes();
            output.extend(value);

            let read_format = perf_event_file.attr.read_format;

            if (read_format & perf_event_read_format_PERF_FORMAT_TOTAL_TIME_ENABLED as u64) != 0 {
                // Total time (ns) event was enabled and running (currently same as TIME_RUNNING).
                output.extend(total_time_running_including_curr.to_ne_bytes());
            }
            if (read_format & perf_event_read_format_PERF_FORMAT_TOTAL_TIME_RUNNING as u64) != 0 {
                // Total time (ns) event was enabled and running (currently same as TIME_ENABLED).
                output.extend(total_time_running_including_curr.to_ne_bytes());
            }
            if (read_format & perf_event_read_format_PERF_FORMAT_ID as u64) != 0 {
                output.extend(perf_event_file.rf_id.to_ne_bytes());
            }

            output
        };

        // The regular read() call allows the case where the bytes-we-want-to-read-in won't
        // fit in the output buffer. However, for perf_event_open's read(), "If you attempt to read
        // into a buffer that is not big enough to hold the data, the error ENOSPC results."
        if data.available() < read_format_data.len() {
            return error!(ENOSPC);
        }
        track_stub!(
            TODO("https://fxbug.dev/402453955"),
            "[perf_event_open] implement remaining error handling"
        );

        data.write(&read_format_data)
    }

    fn ioctl(
        &self,
        _locked: &mut Locked<'_, Unlocked>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        op: u32,
        _arg: SyscallArg,
    ) -> Result<SyscallResult, Errno> {
        track_stub!(
            TODO("https://fxbug.dev/405463320"),
            "[perf_event_open] implement PERF_IOC_FLAG_GROUP"
        );
        let mut perf_event_file = self.perf_event_file.write();
        match op {
            PERF_EVENT_IOC_ENABLE => {
                if perf_event_file.disabled != 0 {
                    perf_event_file.disabled = 0; // 0 = false.
                    perf_event_file.most_recent_enabled_time =
                        zx::MonotonicInstant::get().into_nanos() as u64;
                }
                return Ok(SUCCESS);
            }
            PERF_EVENT_IOC_DISABLE => {
                if perf_event_file.disabled == 0 {
                    perf_event_file.disabled = 1; // 1 = true.

                    // Update total_time_running now that the segment has ended.
                    let curr_time = zx::MonotonicInstant::get().into_nanos() as u64;
                    perf_event_file.total_time_running +=
                        curr_time - perf_event_file.most_recent_enabled_time;
                }
                return Ok(SUCCESS);
            }
            PERF_EVENT_IOC_RESET => {
                perf_event_file.rf_value = 0;
                return Ok(SUCCESS);
            }
            PERF_EVENT_IOC_REFRESH
            | PERF_EVENT_IOC_PERIOD
            | PERF_EVENT_IOC_SET_OUTPUT
            | PERF_EVENT_IOC_SET_FILTER
            | PERF_EVENT_IOC_ID
            | PERF_EVENT_IOC_SET_BPF
            | PERF_EVENT_IOC_PAUSE_OUTPUT
            | PERF_EVENT_IOC_MODIFY_ATTRIBUTES
            | PERF_EVENT_IOC_QUERY_BPF => {
                track_stub!(
                    TODO("https://fxbug.dev/404941053"),
                    "[perf_event_open] implement remaining ioctl() calls"
                );
                return error!(EINVAL);
            }
            _ => error!(ENOTTY),
        }
    }

    fn write(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        _data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        track_stub!(
            TODO("https://fxbug.dev/394960158"),
            "[perf_event_open] implement perf event functions"
        );
        error!(ENOSYS)
    }
}

pub fn sys_perf_event_open(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    attr: UserRef<perf_event_attr>,
    pid: pid_t,
    cpu: i32,
    group_fd: FdNumber,
    _flags: u64,
) -> Result<SyscallResult, Errno> {
    if pid == -1 && cpu == -1 {
        return error!(EINVAL);
    }
    if group_fd != FdNumber::from_raw(-1) {
        track_stub!(TODO("https://fxbug.dev/409619971"), "[perf_event_open] implement group_fd");
        return error!(ENOSYS);
    }
    if pid > 0 {
        track_stub!(TODO("https://fxbug.dev/409621963"), "[perf_event_open] implement pid > 0");
        return error!(ENOSYS);
    }

    // So far, the implementation only sets the read_data_format according to the "Reading results"
    // section of https://man7.org/linux/man-pages/man2/perf_event_open.2.html for a single event.
    // Other features will be added in the future (see below track_stubs).
    let perf_event_attrs: perf_event_attr = current_task.read_object(attr)?;
    let mut perf_event_file = PerfEventFileState {
        attr: perf_event_attrs,
        disabled: perf_event_attrs.disabled(),
        rf_value: 0,
        ..Default::default()
    };

    let read_format = perf_event_attrs.read_format;

    if (read_format & perf_event_read_format_PERF_FORMAT_TOTAL_TIME_ENABLED as u64) != 0
        || (read_format & perf_event_read_format_PERF_FORMAT_TOTAL_TIME_RUNNING as u64) != 0
    {
        // Only keep track of most_recent_enabled_time if we are currently in ENABLED state,
        // as otherwise this param shouldn't be used for calculating anything.
        if perf_event_file.disabled == 0 {
            perf_event_file.most_recent_enabled_time =
                zx::MonotonicInstant::get().into_nanos() as u64;
        }
        // Initialize this to 0 as we will need to return a time duration later during read().
        perf_event_file.total_time_running = 0;
    }
    if (read_format & perf_event_read_format_PERF_FORMAT_ID as u64) != 0 {
        // Adds a 64-bit unique value that corresponds to the event group.
        perf_event_file.rf_id = READ_FORMAT_ID_GENERATOR.fetch_add(1, Ordering::Relaxed);
    }
    if (read_format & perf_event_read_format_PERF_FORMAT_GROUP as u64) != 0 {
        track_stub!(
            TODO("https://fxbug.dev/402238049"),
            "[perf_event_open] implement read_format group"
        );
        return error!(ENOSYS);
    }
    if (read_format & perf_event_read_format_PERF_FORMAT_LOST as u64) != 0 {
        track_stub!(
            TODO("https://fxbug.dev/402260383"),
            "[perf_event_open] implement read_format lost"
        );
    }

    let file = Box::new(PerfEventFile {
        _pid: pid,
        _cpu: cpu,
        perf_event_file: RwLock::new(perf_event_file),
    });
    // TODO: https://fxbug.dev/404739824 - Confirm whether to handle this as a "private" node.
    let file_handle = Anon::new_private_file(current_task, file, OpenFlags::RDWR, "[perf_event]");
    let file_descriptor = current_task.add_file(file_handle, FdFlags::empty());

    Ok(file_descriptor?.into())
}
// Syscalls for arch32 usage
#[cfg(feature = "arch32")]
mod arch32 {
    pub use super::sys_perf_event_open as sys_arch32_perf_event_open;
}

#[cfg(feature = "arch32")]
pub use arch32::*;

use crate::mm::MemoryAccessorExt;
use crate::task::CurrentTask;
use crate::vfs::{Anon, FdFlags, FdNumber, FileObject, FileOps, InputBuffer, OutputBuffer};
use crate::{fileops_impl_nonseekable, fileops_impl_noop_sync};
