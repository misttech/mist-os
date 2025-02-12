// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::DeviceMode;
use crate::fs::proc::cgroups::cgroups_node;
use crate::fs::proc::cpuinfo::CpuinfoFile;
use crate::fs::proc::device_tree::device_tree_node;
use crate::fs::proc::devices::devices_node;
use crate::fs::proc::kallsyms::kallsyms_node;
use crate::fs::proc::kmsg::kmsg_node;
use crate::fs::proc::meminfo::meminfo_node;
use crate::fs::proc::pid_directory::pid_directory;
use crate::fs::proc::pressure_directory::pressure_directory;
use crate::fs::proc::self_symlink::self_node;
use crate::fs::proc::sysctl::{net_directory, sysctl_directory};
use crate::fs::proc::sysrq::SysRqNode;
use crate::fs::proc::thread_self::thread_self_node;
use crate::mm::PAGE_SIZE;
use crate::task::{CurrentTask, Kernel, KernelStats, TaskStateCode};
use crate::vfs::{
    emit_dotdot, fileops_impl_directory, fileops_impl_noop_sync, fs_node_impl_dir_readonly,
    fs_node_impl_symlink, unbounded_seek, BytesFile, BytesFileOps, DirectoryEntryType, DirentSink,
    DynamicFile, DynamicFileBuf, DynamicFileSource, FileObject, FileOps, FileSystemHandle, FsNode,
    FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr, FsString, SeekTarget, StaticDirectoryBuilder,
    StubEmptyFile, SymlinkTarget,
};

use maplit::btreemap;
use starnix_logging::{bug_ref, log_error, track_stub};
use starnix_sync::{FileOpsCore, Locked};
use starnix_types::time::duration_to_scheduler_clock;
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::{DeviceType, MISC_MAJOR};
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::version::{KERNEL_RELEASE, KERNEL_VERSION};
use starnix_uapi::{errno, off_t, pid_t};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::{Arc, Weak};
use std::time::SystemTime;

/// `ProcDirectory` represents the top-level directory in `procfs`.
///
/// It contains, for example, a directory for each running task, named after the task's pid.
///
/// It also contains a special symlink, `self`, which targets the task directory for the task
/// that reads the symlink.
pub struct ProcDirectory {
    /// A map that stores all the nodes that aren't task directories.
    nodes: BTreeMap<&'static FsStr, FsNodeHandle>,
}

impl ProcDirectory {
    /// Returns a new `ProcDirectory` exposing information about `kernel`.
    pub fn new(current_task: &CurrentTask, fs: &FileSystemHandle) -> Arc<ProcDirectory> {
        let kernel = current_task.kernel();

        let mut nodes = btreemap! {
            "cpuinfo".into() => fs.create_node(
                current_task,
                CpuinfoFile::new_node(),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "cmdline".into() => {
                let mut cmdline = Vec::from(kernel.cmdline.clone());
                cmdline.push(b'\n');
                fs.create_node(
                    current_task,
                    BytesFile::new_node(cmdline),
                    FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
                )
            },
            "devices".into() => devices_node(current_task, fs),
            "device-tree".into() => device_tree_node(current_task, fs),
            "self".into() => self_node(current_task, fs),
            "thread-self".into() => thread_self_node(current_task, fs),
            "meminfo".into() => meminfo_node(current_task, fs),
            "kmsg".into() => kmsg_node(current_task, fs),
            "kallsyms".into() => kallsyms_node(current_task, fs),
            "mounts".into() => MountsSymlink::new_node(current_task, fs),
            "cgroups".into() => cgroups_node(current_task, fs),
            "stat".into() => fs.create_node(
                current_task,
                StatFile::new_node(&kernel.stats),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "swaps".into() => fs.create_node(
                current_task,
                SwapsFile::new_node(kernel),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "sys".into() => sysctl_directory(current_task, fs),
            "net".into() => net_directory(current_task, fs),
            "uptime".into() => fs.create_node(
                current_task,
                UptimeFile::new_node(&kernel.stats),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "loadavg".into() => fs.create_node(
                current_task,
                LoadavgFile::new_node(kernel),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "config.gz".into() => fs.create_node(
                current_task,
                ConfigFile::new_node(),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "sysrq-trigger".into() => fs.create_node(
                current_task,
                SysRqNode::new(kernel),
                // This file is normally writable only by root.
                // (https://man7.org/linux/man-pages/man5/proc.5.html)
                FsNodeInfo::new_factory(mode!(IFREG, 0o200), FsCred::root()),
            ),
            "asound".into() => fs.create_node(
                current_task,
                // Note: this is actually a directory but for now just track when it's opened.
                StubEmptyFile::new_node("/proc/asound", bug_ref!("https://fxbug.dev/322893329")),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "diskstats".into() => fs.create_node(
                current_task,
                StubEmptyFile::new_node("/proc/diskstats", bug_ref!("https://fxbug.dev/322893370")),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "filesystems".into() => fs.create_node(
                current_task,
                BytesFile::new_node(b"fxfs".to_vec()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "misc".into() => fs.create_node(
                current_task,
                MiscFile::new_node(),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "modules".into() => fs.create_node(
                current_task,
                // Starnix does not support dynamically loading modules.
                // Instead, we pretend to have loaded a single module, ferris (named after
                // Rust's 🦀), to avoid breaking code that assumes the modules list is
                // non-empty.
                BytesFile::new_node(b"ferris 8192 0 - Live 0x0000000000000000\n".to_vec()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "pagetypeinfo".into() => fs.create_node(
                current_task,
                StubEmptyFile::new_node(
                    "/proc/pagetypeinfo",
                    bug_ref!("https://fxbug.dev/322894315"),
                ),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "slabinfo".into() => fs.create_node(
                current_task,
                StubEmptyFile::new_node("/proc/slabinfo", bug_ref!("https://fxbug.dev/322894195")),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "uid_cputime".into() => {
                let mut dir = StaticDirectoryBuilder::new(fs);
                dir.entry(
                    current_task,
                    "remove_uid_range",
                    StubEmptyFile::new_node(
                        "/proc/uid_cputime/remove_uid_range",
                        bug_ref!("https://fxbug.dev/322894025"),
                    ),
                    mode!(IFREG, 0o222),
                );
                dir.entry(
                    current_task,
                    "show_uid_stat",
                    StubEmptyFile::new_node(
                        "/proc/uid_cputime/show_uid_stat",
                        bug_ref!("https://fxbug.dev/322893886"),
                    ),
                    mode!(IFREG, 0444),
                );
                dir.build(current_task)
            },
            "uid_io".into() => {
                let mut dir = StaticDirectoryBuilder::new(fs);
                dir.entry(
                    current_task,
                    "stats",
                    StubEmptyFile::new_node(
                        "/proc/uid_io/stats",
                        bug_ref!("https://fxbug.dev/322893966"),
                    ),
                    mode!(IFREG, 0o444),
                );
                dir.build(current_task)
            },
            "uid_procstat".into() => {
                let mut dir = StaticDirectoryBuilder::new(fs);
                dir.entry(
                    current_task,
                    "set",
                    StubEmptyFile::new_node(
                        "/proc/uid_procstat/set",
                        bug_ref!("https://fxbug.dev/322894041"),
                    ),
                    mode!(IFREG, 0o222),
                );
                dir.build(current_task)
            },
            "version".into() => fs.create_node(
                current_task,
                BytesFile::new_node(|| {
                    let release = KERNEL_RELEASE;
                    let user = "build-user@build-host";
                    let toolchain = "clang version HEAD, LLD HEAD";
                    let version = KERNEL_VERSION;
                    Ok(format!("Linux version {} ({}) ({}) {}\n", release, user, toolchain, version))
                }),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "vmallocinfo".into() => fs.create_node(
                current_task,
                StubEmptyFile::new_node(
                    "/proc/vmallocinfo",
                    bug_ref!("https://fxbug.dev/322894183"),
                ),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "vmstat".into() => fs.create_node(
                current_task,
                VmStatFile::new_node(&kernel.stats),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
            "zoneinfo".into() => fs.create_node(
                current_task,
                ZoneInfoFile::new_node(&kernel.stats),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
        };

        if let Some(pressure_directory) = pressure_directory(current_task, fs) {
            nodes.insert("pressure".into(), pressure_directory);
        }

        Arc::new(ProcDirectory { nodes })
    }
}

impl FsNodeOps for Arc<ProcDirectory> {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(self.clone()))
    }

    fn lookup(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        match self.nodes.get(name) {
            Some(node) => Ok(Arc::clone(node)),
            None => {
                let pid_string = std::str::from_utf8(name).map_err(|_| errno!(ENOENT))?;
                let pid = pid_string.parse::<pid_t>().map_err(|_| errno!(ENOENT))?;
                let weak_task = current_task.get_task(pid);
                let task = weak_task.upgrade().ok_or_else(|| errno!(ENOENT))?;
                let mut pd_state = task.proc_pid_directory_cache.lock();
                if let Some(pd) = &*pd_state {
                    Ok(pd.clone())
                } else {
                    let pd = pid_directory(current_task, &node.fs(), &task);
                    *pd_state = Some(pd.clone());
                    Ok(pd)
                }
            }
        }
    }
}

impl FileOps for ProcDirectory {
    fileops_impl_directory!();
    fileops_impl_noop_sync!();

    fn seek(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        current_offset: off_t,
        target: SeekTarget,
    ) -> Result<off_t, Errno> {
        unbounded_seek(current_offset, target)
    }

    fn readdir(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        emit_dotdot(file, sink)?;

        // Iterate through all the named entries (i.e., non "task directories") and add them to
        // the sink. Subtract 2 from the offset, to account for `.` and `..`.
        for (name, node) in self.nodes.iter().skip((sink.offset() - 2) as usize) {
            sink.add(
                node.node_id,
                sink.offset() + 1,
                DirectoryEntryType::from_mode(node.info().mode),
                name,
            )?;
        }

        // Add 2 to the number of non-"task directories", to account for `.` and `..`.
        let pid_offset = (self.nodes.len() + 2) as i32;

        // Adjust the offset to account for the other nodes in the directory.
        let adjusted_offset = (sink.offset() - pid_offset as i64) as usize;
        // Sort the pids, to keep the traversal order consistent.
        let mut pids = current_task.kernel().pids.read().process_ids();
        pids.sort();

        // The adjusted offset is used to figure out which task directories are to be listed.
        if let Some(start) = pids.iter().position(|pid| *pid as usize >= adjusted_offset) {
            for pid in &pids[start..] {
                // TODO: Figure out if this inode number is fine, given the content of the task
                // directories.
                let inode_num = file.fs.next_node_id();
                let name = FsString::from(format!("{pid}"));

                // The + 1 is to set the offset to the next possible pid for subsequent reads.
                let next_offset = (*pid + pid_offset + 1) as i64;
                sink.add(inode_num, next_offset, DirectoryEntryType::DIR, name.as_ref())?;
            }
        }

        Ok(())
    }
}

/// A node that represents a link to `self/mounts`.
struct MountsSymlink;

impl MountsSymlink {
    fn new_node(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
        fs.create_node(
            current_task,
            Self,
            FsNodeInfo::new_factory(mode!(IFLNK, 0o777), FsCred::root()),
        )
    }
}

impl FsNodeOps for MountsSymlink {
    fs_node_impl_symlink!();

    fn readlink(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
    ) -> Result<SymlinkTarget, Errno> {
        Ok(SymlinkTarget::Path("self/mounts".into()))
    }
}

#[derive(Clone, Debug)]
struct ConfigFile;
impl ConfigFile {
    pub fn new_node() -> impl FsNodeOps {
        DynamicFile::new_node(Self)
    }
}
impl DynamicFileSource for ConfigFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let contents = std::fs::read("/pkg/data/config.gz").map_err(|e| {
            log_error!("Error reading /pkg/data/config.gz: {e}");
            errno!(EIO)
        })?;
        sink.write(&contents);
        Ok(())
    }
}

#[derive(Clone, Debug)]
struct MiscFile;
impl MiscFile {
    pub fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self)
    }
}
impl BytesFileOps for MiscFile {
    fn read(&self, current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        let registery = &current_task.kernel().device_registry;
        let devices = registery.list_minor_devices(
            DeviceMode::Char,
            DeviceType::new_range(MISC_MAJOR, DeviceMode::Char.minor_range()),
        );
        let mut contents = String::new();
        for (device_type, name) in devices {
            contents.push_str(&format!("{:3} {}\n", device_type.minor(), name));
        }
        Ok(contents.into_bytes().into())
    }
}

#[derive(Clone)]
struct UptimeFile {
    kernel_stats: Arc<KernelStats>,
}

impl UptimeFile {
    pub fn new_node(kernel_stats: &Arc<KernelStats>) -> impl FsNodeOps {
        DynamicFile::new_node(Self { kernel_stats: kernel_stats.clone() })
    }
}

impl DynamicFileSource for UptimeFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let uptime = (zx::MonotonicInstant::get() - zx::MonotonicInstant::ZERO).into_seconds_f64();

        // Fetch CPU stats from `fuchsia.kernel.Stats` to calculate idle time.
        let cpu_stats = self
            .kernel_stats
            .get()
            .get_cpu_stats(zx::MonotonicInstant::INFINITE)
            .map_err(|_| errno!(EIO))?;
        let per_cpu_stats = cpu_stats.per_cpu_stats.unwrap_or(vec![]);
        let idle_time = per_cpu_stats.iter().map(|s| s.idle_time.unwrap_or(0)).sum();
        let idle_time = zx::MonotonicDuration::from_nanos(idle_time).into_seconds_f64();

        writeln!(sink, "{:.2} {:.2}", uptime, idle_time)?;

        Ok(())
    }
}

#[derive(Clone)]
struct ZoneInfoFile {
    kernel_stats: Arc<KernelStats>,
}

impl ZoneInfoFile {
    pub fn new_node(kernel_stats: &Arc<KernelStats>) -> impl FsNodeOps {
        DynamicFile::new_node(Self { kernel_stats: kernel_stats.clone() })
    }
}

impl DynamicFileSource for ZoneInfoFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let mem_stats = self
            .kernel_stats
            .get()
            .get_memory_stats_extended(zx::MonotonicInstant::INFINITE)
            .map_err(|e| {
                log_error!("FIDL error getting memory stats: {e}");
                errno!(EIO)
            })?;

        let userpager_total = mem_stats.vmo_pager_total_bytes.unwrap_or_default() / *PAGE_SIZE;
        let userpager_active = mem_stats.vmo_pager_newest_bytes.unwrap_or_default() / *PAGE_SIZE;

        let nr_active_file = userpager_active;
        let nr_inactive_file = userpager_total.saturating_sub(userpager_active);
        let free = mem_stats.free_bytes.unwrap_or_default() / *PAGE_SIZE;
        let present = mem_stats.total_bytes.unwrap_or_default() / *PAGE_SIZE;

        // Pages min: minimum number of free pages the kernel tries to maintain in this memory zone.
        // Can be set by writing to `/proc/sys/vm/min_free_kbytes`. It is observed to be ~3% of the
        // total memory.
        let pages_min = present * 3 / 100;
        // Pages low: more aggressive memory reclaimation when free pages fall below this level.
        // Typically ~4% of the total memory.
        let pages_low = present * 4 / 100;
        // Pages high: page reclamation begins when free pages drop below this level.
        // Typically ~4% of the total memory.
        let pages_high = present * 6 / 100;

        // Only required fields are written. Add more fields as needed.
        writeln!(sink, "Node 0, zone   Normal")?;
        writeln!(sink, "  per-node stats")?;
        writeln!(sink, "      nr_inactive_file {}", nr_inactive_file)?;
        writeln!(sink, "      nr_active_file {}", nr_active_file)?;
        writeln!(sink, "  pages free     {}", free)?;
        writeln!(sink, "        min      {}", pages_min)?;
        writeln!(sink, "        low      {}", pages_low)?;
        writeln!(sink, "        high     {}", pages_high)?;
        writeln!(sink, "        present  {}", present)?;
        writeln!(sink, "  pagesets")?;

        Ok(())
    }
}

#[derive(Clone)]
struct VmStatFile {
    kernel_stats: Arc<KernelStats>,
}

impl VmStatFile {
    pub fn new_node(kernel_stats: &Arc<KernelStats>) -> impl FsNodeOps {
        DynamicFile::new_node(Self { kernel_stats: kernel_stats.clone() })
    }
}

impl DynamicFileSource for VmStatFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let mem_stats = self
            .kernel_stats
            .get()
            .get_memory_stats_extended(zx::MonotonicInstant::INFINITE)
            .map_err(|e| {
                log_error!("FIDL error getting memory stats: {e}");
                errno!(EIO)
            })?;

        let userpager_total = mem_stats.vmo_pager_total_bytes.unwrap_or_default() / *PAGE_SIZE;
        let userpager_active = mem_stats.vmo_pager_newest_bytes.unwrap_or_default() / *PAGE_SIZE;

        let nr_active_file = userpager_active;
        let nr_inactive_file = userpager_total.saturating_sub(userpager_active);

        // Only fields required so far are written. Add more fields as needed.
        writeln!(sink, "workingset_refault_file {}", 0)?;
        writeln!(sink, "nr_inactive_file {}", nr_inactive_file)?;
        writeln!(sink, "nr_active_file {}", nr_active_file)?;
        writeln!(sink, "pgscan_direct {}", 0)?;
        writeln!(sink, "pgscan_kswapd {}", 0)?;

        Ok(())
    }
}

#[derive(Clone)]
struct StatFile {
    kernel_stats: Arc<KernelStats>,
}
impl StatFile {
    pub fn new_node(kernel_stats: &Arc<KernelStats>) -> impl FsNodeOps {
        DynamicFile::new_node(Self { kernel_stats: kernel_stats.clone() })
    }
}
impl DynamicFileSource for StatFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let uptime = zx::MonotonicInstant::get() - zx::MonotonicInstant::ZERO;

        let cpu_stats =
            self.kernel_stats.get().get_cpu_stats(zx::MonotonicInstant::INFINITE).map_err(|e| {
                log_error!("FIDL error getting cpu stats: {e}");
                errno!(EIO)
            })?;

        // Number of values reported per CPU. See `get_cpu_stats_row` below for the list of values.
        const NUM_CPU_STATS: usize = 10;

        let get_cpu_stats_row = |cpu_stats: &fidl_fuchsia_kernel::PerCpuStats| {
            let idle = zx::MonotonicDuration::from_nanos(cpu_stats.idle_time.unwrap_or(0));

            // Assume that all non-idle time is spent in user mode.
            let user = uptime - idle;

            // Zircon currently reports only number of various interrupts, but not the time spent
            // handling them. Return zeros.
            let nice: u64 = 0;
            let system: u64 = 0;
            let iowait: u64 = 0;
            let irq: u64 = 0;
            let softirq: u64 = 0;
            let steal: u64 = 0;
            let quest: u64 = 0;
            let quest_nice: u64 = 0;

            [
                duration_to_scheduler_clock(user) as u64,
                nice,
                system,
                duration_to_scheduler_clock(idle) as u64,
                iowait,
                irq,
                softirq,
                steal,
                quest,
                quest_nice,
            ]
        };
        let per_cpu_stats = cpu_stats.per_cpu_stats.unwrap_or(vec![]);
        let mut cpu_total_row = [0u64; NUM_CPU_STATS];
        for row in per_cpu_stats.iter().map(get_cpu_stats_row) {
            for (i, value) in row.iter().enumerate() {
                cpu_total_row[i] += value
            }
        }

        writeln!(sink, "cpu {}", cpu_total_row.map(|n| n.to_string()).join(" "))?;
        for (i, row) in per_cpu_stats.iter().map(get_cpu_stats_row).enumerate() {
            writeln!(sink, "cpu{} {}", i, row.map(|n| n.to_string()).join(" "))?;
        }

        let context_switches: u64 =
            per_cpu_stats.iter().map(|s| s.context_switches.unwrap_or(0)).sum();
        writeln!(sink, "ctxt {}", context_switches)?;

        let num_interrupts: u64 = per_cpu_stats.iter().map(|s| s.ints.unwrap_or(0)).sum();
        writeln!(sink, "intr {}", num_interrupts)?;

        let epoch_time = zx::MonotonicDuration::from(
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default(),
        );
        let boot_time_epoch = epoch_time - uptime;
        writeln!(sink, "btime {}", boot_time_epoch.into_seconds())?;

        Ok(())
    }
}

#[derive(Clone)]
struct LoadavgFile(Weak<Kernel>);
impl LoadavgFile {
    pub fn new_node(kernel: &Arc<Kernel>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(Arc::downgrade(kernel)))
    }
}
impl DynamicFileSource for LoadavgFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        let (runnable_tasks, existing_tasks, last_pid) = {
            let kernel = self.0.upgrade().ok_or_else(|| errno!(EIO))?;
            let pid_table = kernel.pids.read();

            let curr_tids = pid_table.task_ids();
            let mut runnable_tasks = 0;
            for pid in &curr_tids {
                let weak_task = pid_table.get_task(*pid);
                if let Some(task) = weak_task.upgrade() {
                    if task.state_code() == TaskStateCode::Running {
                        runnable_tasks += 1;
                    }
                };
            }

            let existing_tasks = pid_table.process_ids().len() + curr_tids.len();
            (runnable_tasks, existing_tasks, pid_table.last_pid())
        };

        track_stub!(TODO("https://fxbug.dev/322874486"), "/proc/loadavg load stats");
        writeln!(sink, "0.50 0.50 0.50 {}/{} {}", runnable_tasks, existing_tasks, last_pid)?;
        Ok(())
    }
}

#[derive(Clone)]
// Tuple member is never used
#[allow(dead_code)]
struct SwapsFile(Weak<Kernel>);
impl SwapsFile {
    pub fn new_node(kernel: &Arc<Kernel>) -> impl FsNodeOps {
        DynamicFile::new_node(Self(Arc::downgrade(kernel)))
    }
}
impl DynamicFileSource for SwapsFile {
    fn generate(&self, sink: &mut DynamicFileBuf) -> Result<(), Errno> {
        track_stub!(TODO("https://fxbug.dev/322874154"), "/proc/swaps includes Kernel::swap_files");
        writeln!(sink, "Filename\t\t\t\tType\t\tSize\t\tUsed\t\tPriority")?;
        Ok(())
    }
}
