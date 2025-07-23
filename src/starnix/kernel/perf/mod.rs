// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Context;
use fuchsia_component::client::connect_to_protocol;
use {fidl_fuchsia_cpu_profiler as profiler, fuchsia_async};

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use starnix_logging::{log_info, log_warn, track_stub};
use starnix_sync::{FileOpsCore, Locked, RwLock, Unlocked};
use starnix_syscalls::{SyscallArg, SyscallResult, SUCCESS};
use starnix_uapi::arch32::{
    perf_event_type_PERF_RECORD_SAMPLE, PERF_EVENT_IOC_DISABLE, PERF_EVENT_IOC_ENABLE,
    PERF_EVENT_IOC_ID, PERF_EVENT_IOC_MODIFY_ATTRIBUTES, PERF_EVENT_IOC_PAUSE_OUTPUT,
    PERF_EVENT_IOC_PERIOD, PERF_EVENT_IOC_QUERY_BPF, PERF_EVENT_IOC_REFRESH, PERF_EVENT_IOC_RESET,
    PERF_EVENT_IOC_SET_BPF, PERF_EVENT_IOC_SET_FILTER, PERF_EVENT_IOC_SET_OUTPUT,
    PERF_RECORD_MISC_USER,
};
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::user_address::UserRef;
use starnix_uapi::{
    error, perf_event_attr, perf_event_read_format_PERF_FORMAT_GROUP,
    perf_event_read_format_PERF_FORMAT_ID, perf_event_read_format_PERF_FORMAT_LOST,
    perf_event_read_format_PERF_FORMAT_TOTAL_TIME_ENABLED,
    perf_event_read_format_PERF_FORMAT_TOTAL_TIME_RUNNING, tid_t, uapi,
};
use zx::sys::zx_system_get_page_size;

static READ_FORMAT_ID_GENERATOR: AtomicU64 = AtomicU64::new(0);
// Default sample period of one sample per millisecond.
static DEFAULT_SAMPLE_PERIOD: u64 = 1000000;

uapi::check_arch_independent_layout! {
    perf_event_attr {
        type_, // "type" is a reserved keyword so add a trailing underscore.
        size,
        config,
        __bindgen_anon_1,
        sample_type,
        read_format,
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
    samples_collected: u64,
}

struct PerfEventFile {
    _tid: tid_t,
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
        _locked: &mut Locked<FileOpsCore>,
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
        _locked: &mut Locked<Unlocked>,
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
                return error!(ENOSYS);
            }
            _ => error!(ENOTTY),
        }
    }

    // Gets called when mmap() is called.
    fn get_memory(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        length: Option<usize>,
        _prot: ProtectionFlags,
    ) -> Result<Arc<MemoryObject>, Errno> {
        let buffer_size: u64 = length.unwrap_or(0) as u64;
        if buffer_size == 0 {
            return error!(EINVAL);
        }
        let vmo_object: Result<zx::Vmo, zx::Status> = zx::Vmo::create(buffer_size);
        let vmo = match vmo_object {
            Ok(vmo) => vmo,
            Err(status) => {
                if status == zx::Status::NO_MEMORY {
                    return error!(ENOMEM);
                }
                return error!(EINVAL);
            }
        };

        // Write metadata page. Currently we hardcode everything just to get something E2E working.
        let mut metadata = Vec::<u8>::new();
        // version
        metadata.extend(1_u32.to_ne_bytes());
        // compat version
        metadata.extend(2_u32.to_ne_bytes());
        // lock
        metadata.extend(2_u32.to_ne_bytes());
        // index
        metadata.extend(2_u32.to_ne_bytes());
        // offset
        metadata.extend(19337_i64.to_ne_bytes());
        // time_enabled
        metadata.extend(606868_u64.to_ne_bytes());
        // time_running
        metadata.extend(622968_u64.to_ne_bytes());
        // capabilities
        metadata.extend(30_u64.to_ne_bytes());
        // All the fields between pmc_width and reserved (inclusive).
        metadata.extend(vec![0; 976].as_slice());
        // data_head (see below comment re: PERF_RECORD_SAMPLE).
        metadata.extend(40_u64.to_ne_bytes());
        // data_tail
        metadata.extend(0_u64.to_ne_bytes());
        // data_offset. Don't mind the unsafe block.
        // https://fuchsia.dev/reference/syscalls/system_get_page_size#errors
        // says it cannot fail, but rust compiler needs it.
        let page_size: u64 = unsafe { zx_system_get_page_size() } as u64;
        metadata.extend(page_size.to_ne_bytes());
        // data_size
        metadata.extend(((buffer_size - page_size) as u64).to_ne_bytes());
        // The remaining metadata are not defined for now.

        match vmo.write(&metadata, 0 /* This is the offset, not the length to write */) {
            Ok(()) => {}
            Err(_) => {
                track_stub!(
                    TODO("https://fxbug.dev/416323134"),
                    "[perf_event_open] handle get_memory() errors"
                );
                return error!(EINVAL);
            }
        };

        // Add 1 example sample of type PERF_RECORD_SAMPLE.
        // This sample record includes the perf_event_header and the first 5 fields
        // resulting in a total size of 40 bytes.
        track_stub!(
            TODO("https://fxbug.dev/398914921"),
            "[perf_event_open] incorporate experimental profiler API"
        );
        let mut sample = Vec::<u8>::new();
        // perf_event_header type
        sample.extend((perf_event_type_PERF_RECORD_SAMPLE as u32).to_ne_bytes());
        // perf_event_header misc
        sample.extend((PERF_RECORD_MISC_USER as u16).to_ne_bytes());
        // perf_event_header size - size of the record including header.
        sample.extend(40_u16.to_ne_bytes());
        // perf_record_sample sample_id
        sample.extend(12_u64.to_ne_bytes());
        // ip
        sample.extend(123_u64.to_ne_bytes());
        // pid
        sample.extend(1234_u32.to_ne_bytes());
        // tid
        sample.extend(12345_u32.to_ne_bytes());
        // time
        sample.extend(123456_u64.to_ne_bytes());
        // The remaining data are not defined for now.

        match vmo.write(&sample, page_size /* offset */) {
            Ok(()) => {
                let memory = MemoryObject::RingBuf(vmo);
                return Ok(Arc::new(memory));
            }
            Err(_) => {
                track_stub!(
                    TODO("https://fxbug.dev/416323134"),
                    "[perf_event_open] handle get_memory() errors"
                );
                return error!(EINVAL);
            }
        };
    }

    fn write(
        &self,
        _locked: &mut Locked<FileOpsCore>,
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

async fn set_up_profiler() -> Result<profiler::SessionProxy, Errno> {
    // Configuration for how we want to sample.
    let sample = profiler::Sample {
        callgraph: Some(profiler::CallgraphConfig {
            strategy: Some(profiler::CallgraphStrategy::FramePointer),
            ..Default::default()
        }),
        ..Default::default()
    };

    let sampling_config = profiler::SamplingConfig {
        period: Some(DEFAULT_SAMPLE_PERIOD),
        timebase: Some(profiler::Counter::PlatformIndependent(profiler::CounterId::Nanoseconds)),
        sample: Some(sample),
        ..Default::default()
    };

    let tasks = vec![
        // Should return a value around 1000-5000 for 5 seconds.
        profiler::Task::SystemWide(profiler::SystemWide {}),
    ];
    let targets = profiler::TargetConfig::Tasks(tasks);
    let config = profiler::Config {
        configs: Some(vec![sampling_config]),
        target: Some(targets),
        ..Default::default()
    };
    let (_, server) = fidl::Socket::create_stream();
    let configure = profiler::SessionConfigureRequest {
        output: Some(server),
        config: Some(config),
        ..Default::default()
    };

    let proxy = connect_to_protocol::<profiler::SessionMarker>()
        .context("Error connecting to Profiler protocol");
    let session_proxy: profiler::SessionProxy = match proxy {
        Ok(p) => p.clone(),
        Err(e) => return error!(EINVAL, e),
    };

    // Must configure before sampling start().
    let config_request = session_proxy.configure(configure).await;
    match config_request {
        Ok(_) => Ok(session_proxy),
        Err(e) => return error!(EINVAL, e),
    }
}

async fn collect_sample(
    session_proxy: profiler::SessionProxy,
    seconds: Duration,
) -> Result<u64, Errno> {
    let start_request = profiler::SessionStartRequest {
        buffer_results: Some(true),
        buffer_size_mb: Some(8 as u64),
        ..Default::default()
    };
    let _ = session_proxy.start(&start_request).await.expect("Failed to start profiling");

    // Hardcode a duration so that samples can be collected. This is currently solely used to
    // demonstrate that an E2E implementation of sample collection works.
    track_stub!(
        TODO("https://fxbug.dev/428974888"),
        "[perf_event_open] don't hardcode sleep; test/user should decide sample duration"
    );
    let _ = fuchsia_async::Timer::new(seconds).await;

    let stats = session_proxy.stop().await;
    let samples_collected = match stats {
        Ok(stats) => stats.samples_collected.unwrap(),
        Err(e) => return error!(EINVAL, e),
    };

    track_stub!(
        TODO("https://fxbug.dev/422502681"),
        "[perf_event_open] symbolize sample output and delete the println"
    );
    log_info!("profiler samples_collected: {:?}", samples_collected);
    let reset_status = session_proxy.reset().await;
    return match reset_status {
        Ok(_) => Ok(samples_collected),
        Err(e) => {
            log_warn!("Failed to reset profiler session due to {}", e);
            Ok(samples_collected)
        }
    };
}

pub fn sys_perf_event_open(
    locked: &mut Locked<Unlocked>,
    current_task: &CurrentTask,
    attr: UserRef<perf_event_attr>,
    // Note that this is pid in Linux docs.
    tid: tid_t,
    cpu: i32,
    group_fd: FdNumber,
    _flags: u64,
) -> Result<SyscallResult, Errno> {
    if tid == -1 && cpu == -1 {
        return error!(EINVAL);
    }
    if group_fd != FdNumber::from_raw(-1) {
        track_stub!(TODO("https://fxbug.dev/409619971"), "[perf_event_open] implement group_fd");
        return error!(ENOSYS);
    }
    if tid > 0 {
        track_stub!(TODO("https://fxbug.dev/409621963"), "[perf_event_open] implement tid > 0");
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

    // If we are sampling, invoke the profiler and collect a sample.
    // Currently this is an example sample collection.
    track_stub!(
        TODO("https://fxbug.dev/398914921"),
        "[perf_event_open] implement full sampling features"
    );
    unsafe {
        if perf_event_attrs.__bindgen_anon_1.sample_period > 0 {
            let mut executor = fuchsia_async::LocalExecutor::new();
            executor.run_singlethreaded(async {
                let session_proxy =
                    set_up_profiler().await.expect("Failed to get session proxy for profiler");
                let samples_collected = collect_sample(session_proxy, Duration::from_secs(5)).await;
                perf_event_file.samples_collected =
                    samples_collected.expect("Failed to collecte samples");
            });
        }
    }

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
        _tid: tid,
        _cpu: cpu,
        perf_event_file: RwLock::new(perf_event_file),
    });
    // TODO: https://fxbug.dev/404739824 - Confirm whether to handle this as a "private" node.
    let file_handle =
        Anon::new_private_file(locked, current_task, file, OpenFlags::RDWR, "[perf_event]");
    let file_descriptor: Result<FdNumber, Errno> =
        current_task.add_file(locked, file_handle, FdFlags::empty());

    match file_descriptor {
        Ok(fd) => Ok(fd.into()),
        Err(_) => {
            track_stub!(
                TODO("https://fxbug.dev/402453955"),
                "[perf_event_open] implement remaining error handling"
            );
            error!(EMFILE)
        }
    }
}
// Syscalls for arch32 usage
#[cfg(feature = "arch32")]
mod arch32 {
    pub use super::sys_perf_event_open as sys_arch32_perf_event_open;
}

#[cfg(feature = "arch32")]
pub use arch32::*;

use crate::mm::memory::MemoryObject;
use crate::mm::{MemoryAccessorExt, ProtectionFlags};
use crate::task::CurrentTask;
use crate::vfs::{Anon, FdFlags, FdNumber, FileObject, FileOps, InputBuffer, OutputBuffer};
use crate::{fileops_impl_nonseekable, fileops_impl_noop_sync};
