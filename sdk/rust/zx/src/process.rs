// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Type-safe bindings for Zircon processes.

use crate::sys::{self as sys, zx_handle_t, zx_time_t, ZX_OBJ_TYPE_UPPER_BOUND};
use crate::{
    object_get_info_single, object_get_info_vec, object_get_property, object_set_property, ok,
    AsHandleRef, Handle, HandleBased, HandleRef, Koid, MapInfo, ObjectQuery, Property,
    PropertyQuery, Rights, Status, Task, Thread, Topic, VmoInfo,
};
use bitflags::bitflags;
use std::mem::MaybeUninit;

bitflags! {
    /// Options that may be used when creating a `Process`.
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct ProcessOptions: u32 {
        const SHARED = sys::ZX_PROCESS_SHARED;
    }
}

impl Default for ProcessOptions {
    fn default() -> Self {
        ProcessOptions::empty()
    }
}

bitflags! {
    #[repr(transparent)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct ProcessInfoFlags: u32 {
        const STARTED = sys::ZX_INFO_PROCESS_FLAG_STARTED;
        const EXITED = sys::ZX_INFO_PROCESS_FLAG_EXITED;
        const DEBUGGER_ATTACHED = sys::ZX_INFO_PROCESS_FLAG_DEBUGGER_ATTACHED;
    }
}

/// An object representing a Zircon process.
///
/// As essentially a subtype of `Handle`, it can be freely interconverted.
#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
#[repr(transparent)]
pub struct Process(Handle);
impl_handle_based!(Process);
unsafe_handle_properties!(object: Process,
    props: [
        {query_ty: PROCESS_DEBUG_ADDR, tag: ProcessDebugAddrTag, prop_ty: u64, get:get_debug_addr, set:set_debug_addr},
        {query_ty: PROCESS_BREAK_ON_LOAD, tag: ProcessBreakOnLoadTag, prop_ty: u64, get:get_break_on_load, set:set_break_on_load},
    ]
);

sys::zx_info_process_t!(ProcessInfo);

impl From<sys::zx_info_process_t> for ProcessInfo {
    fn from(info: sys::zx_info_process_t) -> ProcessInfo {
        let sys::zx_info_process_t { return_code, start_time, flags } = info;
        ProcessInfo { return_code, start_time, flags }
    }
}

// ProcessInfo is able to be safely replaced with a byte representation and is a PoD type.
unsafe impl ObjectQuery for ProcessInfo {
    const TOPIC: Topic = Topic::PROCESS;
    type InfoTy = ProcessInfo;
}

struct ProcessThreadsInfo;

// ProcessThreadsInfo is able to be safely replaced with a byte representation and is a PoD type.
unsafe impl ObjectQuery for ProcessThreadsInfo {
    const TOPIC: Topic = Topic::PROCESS_THREADS;
    type InfoTy = Koid;
}

sys::zx_info_task_stats_t!(TaskStatsInfo);

impl From<sys::zx_info_task_stats_t> for TaskStatsInfo {
    fn from(
        sys::zx_info_task_stats_t {
            mem_mapped_bytes,
            mem_private_bytes,
            mem_shared_bytes,
            mem_scaled_shared_bytes,
            mem_fractional_scaled_shared_bytes,
        }: sys::zx_info_task_stats_t,
    ) -> TaskStatsInfo {
        TaskStatsInfo {
            mem_mapped_bytes,
            mem_private_bytes,
            mem_shared_bytes,
            mem_scaled_shared_bytes,
            mem_fractional_scaled_shared_bytes,
        }
    }
}

// TaskStatsInfo is able to be safely replaced with a byte representation and is a PoD type.
unsafe impl ObjectQuery for TaskStatsInfo {
    const TOPIC: Topic = Topic::TASK_STATS;
    type InfoTy = TaskStatsInfo;
}

struct ProcessMapsInfo;
unsafe impl ObjectQuery for ProcessMapsInfo {
    const TOPIC: Topic = Topic::PROCESS_MAPS;
    type InfoTy = sys::zx_info_maps_t;
}

struct ProcessVmoInfo;
unsafe impl ObjectQuery for ProcessVmoInfo {
    const TOPIC: Topic = Topic::PROCESS_VMOS;
    type InfoTy = sys::zx_info_vmo_t;
}

sys::zx_info_process_handle_stats_t!(ProcessHandleStats);

impl Default for ProcessHandleStats {
    fn default() -> Self {
        Self { handle_count: [0; ZX_OBJ_TYPE_UPPER_BOUND] }
    }
}

unsafe impl ObjectQuery for ProcessHandleStats {
    const TOPIC: Topic = Topic::PROCESS_HANDLE_STATS;
    type InfoTy = ProcessHandleStats;
}

impl Process {
    /// Similar to `Thread::start`, but is used to start the first thread in a process.
    ///
    /// Wraps the
    /// [zx_process_start](https://fuchsia.dev/fuchsia-src/reference/syscalls/process_start.md)
    /// syscall.
    pub fn start(
        &self,
        thread: &Thread,
        entry: usize,
        stack: usize,
        arg1: Handle,
        arg2: usize,
    ) -> Result<(), Status> {
        let process_raw = self.raw_handle();
        let thread_raw = thread.raw_handle();
        let arg1 = arg1.into_raw();
        ok(unsafe { sys::zx_process_start(process_raw, thread_raw, entry, stack, arg1, arg2) })
    }

    /// Create a thread inside a process.
    ///
    /// Wraps the
    /// [zx_thread_create](https://fuchsia.dev/fuchsia-src/reference/syscalls/thread_create.md)
    /// syscall.
    pub fn create_thread(&self, name: &[u8]) -> Result<Thread, Status> {
        let process_raw = self.raw_handle();
        let name_ptr = name.as_ptr();
        let name_len = name.len();
        let options = 0;
        let mut thread_out = 0;
        let status = unsafe {
            sys::zx_thread_create(process_raw, name_ptr, name_len, options, &mut thread_out)
        };
        ok(status)?;
        unsafe { Ok(Thread::from(Handle::from_raw(thread_out))) }
    }

    /// Write memory inside a process.
    ///
    /// Wraps the
    /// [zx_process_write_memory](https://fuchsia.dev/fuchsia-src/reference/syscalls/process_write_memory.md)
    /// syscall.
    pub fn write_memory(&self, vaddr: sys::zx_vaddr_t, bytes: &[u8]) -> Result<usize, Status> {
        let mut actual = 0;
        let status = unsafe {
            sys::zx_process_write_memory(
                self.raw_handle(),
                vaddr,
                bytes.as_ptr(),
                bytes.len(),
                &mut actual,
            )
        };
        ok(status).map(|()| actual)
    }

    /// Read memory from inside a process.
    ///
    /// Wraps the
    /// [zx_process_read_memory](https://fuchsia.dev/fuchsia-src/reference/syscalls/process_read_memory.md)
    /// syscall.
    pub fn read_memory(&self, vaddr: sys::zx_vaddr_t, bytes: &mut [u8]) -> Result<usize, Status> {
        // SAFETY: It's OK to interpret &mut [u8] as &mut [MaybeUninit<u8>] as long as we don't
        // expose the MaybeUninit reference to code that would write uninitialized values to
        // elements of the slice. Every valid state for a u8 is also a valid state for
        // MaybeUninit<u8>, although the reverse is not true.
        let (actually_read, _) = self.read_memory_uninit(vaddr, unsafe {
            std::slice::from_raw_parts_mut(
                bytes.as_mut_ptr().cast::<MaybeUninit<u8>>(),
                bytes.len(),
            )
        })?;
        Ok(actually_read.len())
    }

    /// Read memory from inside a process without requiring the output buffer to be initialized.
    ///
    /// Wraps the
    /// [zx_process_read_memory](https://fuchsia.dev/fuchsia-src/reference/syscalls/process_read_memory.md)
    /// syscall.
    pub fn read_memory_uninit<'a>(
        &self,
        vaddr: sys::zx_vaddr_t,
        buffer: &'a mut [MaybeUninit<u8>],
    ) -> Result<(&'a mut [u8], &'a mut [MaybeUninit<u8>]), Status> {
        let mut actually_read = 0;
        // SAFETY: This is a system call that requires the pointers passed are valid to write to.
        // We get the pointers from a valid mutable slice so we know it's safe to ask the kernel to
        // write to them. Casting the *mut MaybeUninit<u8> to a *mut u8 is safe because all valid
        // values for u8 are a subset of the valid values for MaybeUninit<u8> and we know the
        // kernel won't write uninitialized values to the slice.
        let status = unsafe {
            sys::zx_process_read_memory(
                self.raw_handle(),
                vaddr,
                // TODO(https://fxbug.dev/42079723) use MaybeUninit::slice_as_mut_ptr when stable
                buffer.as_mut_ptr().cast::<u8>(),
                buffer.len(),
                &mut actually_read,
            )
        };
        ok(status)?;
        let (initialized, uninitialized) = buffer.split_at_mut(actually_read);
        Ok((
            // TODO(https://fxbug.dev/42079723) use MaybeUninit::slice_assume_init_mut when stable
            // SAFETY: We're converting &mut [MaybeUninit<u8>] back to &mut [u8], which is only
            // valid to do if all elements of `initialized` have actually been initialized. Here we
            // have to trust that the kernel didn't lie when it said it wrote to the entire buffer,
            // but as long as that assumption is valid them it's safe to assume this slice is init.
            unsafe {
                std::slice::from_raw_parts_mut(
                    initialized.as_mut_ptr().cast::<u8>(),
                    initialized.len(),
                )
            },
            uninitialized,
        ))
    }

    /// Wraps the
    /// [zx_object_get_info](https://fuchsia.dev/fuchsia-src/reference/syscalls/object_get_info.md)
    /// syscall for the ZX_INFO_PROCESS topic.
    pub fn info(&self) -> Result<ProcessInfo, Status> {
        object_get_info_single::<ProcessInfo>(self.as_handle_ref())
    }

    /// Wraps the
    /// [zx_object_get_info](https://fuchsia.dev/fuchsia-src/reference/syscalls/object_get_info.md)
    /// syscall for the ZX_INFO_PROCESS_THREADS topic.
    pub fn threads(&self) -> Result<Vec<Koid>, Status> {
        object_get_info_vec::<ProcessThreadsInfo>(self.as_handle_ref())
    }

    /// Wraps the
    /// [zx_object_get_info](https://fuchsia.dev/fuchsia-src/reference/syscalls/object_get_info.md)
    /// syscall for the ZX_INFO_TASK_STATS topic.
    pub fn task_stats(&self) -> Result<TaskStatsInfo, Status> {
        object_get_info_single::<TaskStatsInfo>(self.as_handle_ref())
    }

    /// Wraps the
    /// [zx_object_get_info](https://fuchsia.dev/fuchsia-src/reference/syscalls/object_get_info.md)
    /// syscall for the ZX_INFO_PROCESS_MAPS topic.
    pub fn info_maps_vec(&self) -> Result<Vec<MapInfo>, Status> {
        object_get_info_vec::<ProcessMapsInfo>(self.as_handle_ref())?
            .into_iter()
            .map(|i| {
                // SAFETY: these values were written by the kernel which is the requirement for this
                // function.
                unsafe { MapInfo::from_raw(i) }
            })
            .collect::<Result<Vec<_>, _>>()
    }

    /// Exit the current process with the given return code.
    ///
    /// Wraps the
    /// [zx_process_exit](https://fuchsia.dev/fuchsia-src/reference/syscalls/process_exit.md)
    /// syscall.
    pub fn exit(retcode: i64) -> ! {
        unsafe {
            sys::zx_process_exit(retcode);
            // zither generates the syscall returning a unit value. We know it will not proceed
            // past this point however.
            std::hint::unreachable_unchecked()
        }
    }

    /// Wraps the
    /// [zx_object_get_info](https://fuchsia.dev/fuchsia-src/reference/syscalls/object_get_info.md)
    /// syscall for the ZX_INFO_PROCESS_HANDLE_STATS topic.
    pub fn handle_stats(&self) -> Result<ProcessHandleStats, Status> {
        object_get_info_single::<ProcessHandleStats>(self.as_handle_ref())
    }

    /// Wraps the
    /// [zx_object_get_child](https://fuchsia.dev/fuchsia-src/reference/syscalls/object_get_child.md)
    /// syscall.
    pub fn get_child(&self, koid: &Koid, rights: Rights) -> Result<Thread, Status> {
        let mut handle: zx_handle_t = Default::default();
        let status = unsafe {
            sys::zx_object_get_child(self.raw_handle(), koid.raw_koid(), rights.bits(), &mut handle)
        };
        ok(status)?;
        Ok(Thread::from(unsafe { Handle::from_raw(handle) }))
    }

    /// Wraps the
    /// [zx_object_get_info](https://fuchsia.dev/fuchsia-src/reference/syscalls/object_get_info.md)
    /// syscall for the ZX_INFO_PROCESS_VMO topic.
    pub fn info_vmos_vec(&self) -> Result<Vec<VmoInfo>, Status> {
        let raw_info = object_get_info_vec::<ProcessVmoInfo>(self.as_handle_ref())?;
        Ok(raw_info.into_iter().map(|info| info.into()).collect())
    }
}

impl Task for Process {}

#[cfg(test)]
mod tests {
    use crate::cprng_draw;
    // The unit tests are built with a different crate name, but fdio and fuchsia_runtime return a
    // "real" zx::Process that we need to use.
    use assert_matches::assert_matches;
    use std::ffi::CString;
    use zx::{
        sys, system_get_page_size, AsHandleRef, Handle, Instant, MapDetails, ProcessInfo,
        ProcessInfoFlags, ProcessOptions, Signals, Task, TaskStatsInfo, VmarFlags, Vmo,
    };

    #[test]
    fn info_self() {
        let process = fuchsia_runtime::process_self();
        let info = process.info().unwrap();
        const STARTED: u32 = ProcessInfoFlags::STARTED.bits();
        assert_matches!(
            info,
            ProcessInfo {
                return_code: 0,
                start_time,
                flags: STARTED,
            } if start_time > 0
        );
    }

    #[test]
    fn stats_self() {
        let process = fuchsia_runtime::process_self();
        let task_stats = process.task_stats().unwrap();

        // Values greater than zero should be reported back for all memory usage
        // types.
        assert!(matches!(task_stats,
            TaskStatsInfo {
                mem_mapped_bytes,
                mem_private_bytes,
                mem_shared_bytes,
                mem_scaled_shared_bytes,
                mem_fractional_scaled_shared_bytes: _
            }
            if mem_mapped_bytes > 0
                && mem_private_bytes > 0
                && mem_shared_bytes > 0
                && mem_scaled_shared_bytes > 0));
    }

    #[test]
    fn exit_and_info() {
        let mut randbuf = [0; 8];
        cprng_draw(&mut randbuf);
        let expected_code = i64::from_le_bytes(randbuf);
        let arg = CString::new(format!("{}", expected_code)).unwrap();

        // This test utility will exercise zx::Process::exit, using the provided argument as the
        // return code.
        let binpath = CString::new("/pkg/bin/exit_with_code_util").unwrap();
        let process = fdio::spawn(
            &fuchsia_runtime::job_default(),
            fdio::SpawnOptions::DEFAULT_LOADER,
            &binpath,
            &[&arg],
        )
        .expect("Failed to spawn process");

        process
            .wait_handle(Signals::PROCESS_TERMINATED, Instant::INFINITE)
            .expect("Wait for process termination failed");
        let info = process.info().unwrap();
        const STARTED_AND_EXITED: u32 =
            ProcessInfoFlags::STARTED.bits() | ProcessInfoFlags::EXITED.bits();
        assert_matches!(
            info,
            ProcessInfo {
                return_code,
                start_time,
                flags: STARTED_AND_EXITED,
            } if return_code == expected_code && start_time > 0
        );
    }

    #[test]
    fn kill_and_info() {
        // This test utility will sleep "forever" without exiting, so that we can kill it..
        let binpath = CString::new("/pkg/bin/sleep_forever_util").unwrap();
        let process = fdio::spawn(
            &fuchsia_runtime::job_default(),
            // Careful not to clone stdio here, or the test runner can hang.
            fdio::SpawnOptions::DEFAULT_LOADER,
            &binpath,
            &[&binpath],
        )
        .expect("Failed to spawn process");

        let info = process.info().unwrap();
        const STARTED: u32 = ProcessInfoFlags::STARTED.bits();
        assert_matches!(
            info,
            ProcessInfo {
                return_code: 0,
                start_time,
                flags: STARTED,
             } if start_time > 0
        );

        process.kill().expect("Failed to kill process");
        process
            .wait_handle(Signals::PROCESS_TERMINATED, Instant::INFINITE)
            .expect("Wait for process termination failed");

        let info = process.info().unwrap();
        const STARTED_AND_EXITED: u32 =
            ProcessInfoFlags::STARTED.bits() | ProcessInfoFlags::EXITED.bits();
        assert_matches!(
            info,
            ProcessInfo {
                return_code: sys::ZX_TASK_RETCODE_SYSCALL_KILL,
                start_time,
                flags: STARTED_AND_EXITED,
            } if start_time > 0
        );
    }

    #[test]
    fn maps_info() {
        let root_vmar = fuchsia_runtime::vmar_root_self();
        let process = fuchsia_runtime::process_self();

        // Create two mappings so we know what to expect from our test calls.
        let vmo = Vmo::create(system_get_page_size() as u64).unwrap();
        let vmo_koid = vmo.get_koid().unwrap();

        let map1 = root_vmar
            .map(0, &vmo, 0, system_get_page_size() as usize, VmarFlags::PERM_READ)
            .unwrap();
        let map2 = root_vmar
            .map(0, &vmo, 0, system_get_page_size() as usize, VmarFlags::PERM_READ)
            .unwrap();

        let info = process.info_maps_vec().unwrap();

        // We should find our two mappings in the info.
        let count = info
            .iter()
            .filter(|info| match info.details {
                MapDetails::Mapping(d) => d.vmo_koid == vmo_koid,
                _ => false,
            })
            .count();
        assert_eq!(count, 2);

        // We created these mappings and are not letting any references to them escape so unmapping
        // is safe to do.
        unsafe {
            root_vmar.unmap(map1, system_get_page_size() as usize).unwrap();
            root_vmar.unmap(map2, system_get_page_size() as usize).unwrap();
        }
    }

    #[test]
    fn info_maps_vec() {
        let root_vmar = fuchsia_runtime::vmar_root_self();
        let process = fuchsia_runtime::process_self();

        // Create two mappings so we know what to expect from our test calls.
        let vmo = Vmo::create(system_get_page_size() as u64).unwrap();
        let vmo_koid = vmo.get_koid().unwrap();

        let map1 = root_vmar
            .map(0, &vmo, 0, system_get_page_size() as usize, VmarFlags::PERM_READ)
            .unwrap();
        let map2 = root_vmar
            .map(0, &vmo, 0, system_get_page_size() as usize, VmarFlags::PERM_READ)
            .unwrap();

        let info = process.info_maps_vec().unwrap();

        // We should find our two mappings in the info.
        let count = info
            .iter()
            .filter(|info| match info.details {
                MapDetails::Mapping(d) => d.vmo_koid == vmo_koid,
                _ => false,
            })
            .count();
        assert_eq!(count, 2);

        // We created these mappings and are not letting any references to them escape so unmapping
        // is safe to do.
        unsafe {
            root_vmar.unmap(map1, system_get_page_size() as usize).unwrap();
            root_vmar.unmap(map2, system_get_page_size() as usize).unwrap();
        }
    }

    #[test]
    fn info_vmos_vec() {
        let process = fuchsia_runtime::process_self();

        // Create two mappings so we know what to expect from our test calls.
        let vmo = Vmo::create(system_get_page_size() as u64).unwrap();
        let vmo_koid = vmo.get_koid().unwrap();

        let info = process.info_vmos_vec().unwrap();

        // We should find our two mappings in the info.
        let count = info.iter().filter(|map| map.koid == vmo_koid).count();
        assert_eq!(count, 1);
    }

    #[test]
    fn handle_stats() {
        let process = fuchsia_runtime::process_self();
        let handle_stats = process.handle_stats().unwrap();

        // We don't have an opinion about how many handles a typical process has, or what types
        // they will be (the function returns counts for up to 64 different types),
        // but a reasonable total should be between 1 and a million.
        let sum: u32 = handle_stats.handle_count.iter().sum();

        assert!(sum > 0);
        assert!(sum < 1_000_000);
    }

    #[test]
    fn threads_contain_self() {
        let current_thread_koid = fuchsia_runtime::thread_self().get_koid().unwrap();
        let threads_koids = fuchsia_runtime::process_self().threads().unwrap();
        assert!(threads_koids.contains(&current_thread_koid));
        let thread_handle = fuchsia_runtime::process_self()
            .get_child(&current_thread_koid, zx::Rights::NONE)
            .unwrap();
        assert_eq!(thread_handle.get_koid().unwrap(), current_thread_koid);
    }

    #[test]
    fn new_process_no_threads() {
        let job = fuchsia_runtime::job_default().create_child_job().unwrap();
        let (process, _) =
            job.create_child_process(ProcessOptions::empty(), b"test-process").unwrap();
        assert!(process.threads().unwrap().is_empty());
    }

    #[test]
    fn non_started_threads_dont_show_up() {
        let job = fuchsia_runtime::job_default().create_child_job().unwrap();
        let (process, _) =
            job.create_child_process(ProcessOptions::empty(), b"test-process").unwrap();

        let thread = process.create_thread(b"test-thread").unwrap();
        let thread_koid = thread.get_koid().unwrap();

        assert!(process.threads().unwrap().is_empty());
        assert!(process.get_child(&thread_koid, zx::Rights::NONE).is_err());
    }

    #[test]
    fn started_threads_show_up() {
        let job = fuchsia_runtime::job_default().create_child_job().unwrap();
        let (process, root_vmar) =
            job.create_child_process(ProcessOptions::empty(), b"test-process").unwrap();

        let valid_addr = root_vmar.info().unwrap().base;

        let thread1 = process.create_thread(b"test-thread-1").unwrap();
        let thread2 = process.create_thread(b"test-thread-2").unwrap();

        // start with the thread suspended, so we don't care about executing invalid code.
        let thread1_suspended = thread1.suspend().unwrap();
        process.start(&thread1, valid_addr, valid_addr, Handle::invalid(), 0).unwrap();

        let threads_koids = process.threads().unwrap();
        assert_eq!(threads_koids.len(), 1);
        assert_eq!(threads_koids[0], thread1.get_koid().unwrap());
        assert_eq!(
            process.get_child(&threads_koids[0], zx::Rights::NONE).unwrap().get_koid().unwrap(),
            threads_koids[0]
        );

        // Add another thread.
        let thread2_suspended = thread2.suspend().unwrap();
        thread2.start(valid_addr, valid_addr, 0, 0).unwrap();

        let threads_koids = process.threads().unwrap();
        assert_eq!(threads_koids.len(), 2);
        assert!(threads_koids.contains(&thread1.get_koid().unwrap()));
        assert!(threads_koids.contains(&thread2.get_koid().unwrap()));
        assert_eq!(
            process.get_child(&threads_koids[0], zx::Rights::NONE).unwrap().get_koid().unwrap(),
            threads_koids[0]
        );
        assert_eq!(
            process.get_child(&threads_koids[1], zx::Rights::NONE).unwrap().get_koid().unwrap(),
            threads_koids[1]
        );

        process.kill().unwrap();
        process.wait_handle(Signals::TASK_TERMINATED, Instant::INFINITE).unwrap();

        drop(thread1_suspended);
        drop(thread2_suspended);

        assert!(process.threads().unwrap().is_empty());
    }
}
