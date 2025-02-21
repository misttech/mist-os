// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fs::proc::cgroups::cgroups_node;
use crate::fs::proc::cmdline::cmdline_node;
use crate::fs::proc::config_gz::config_gz_node;
use crate::fs::proc::cpuinfo::cpuinfo_node;
use crate::fs::proc::device_tree::device_tree_node;
use crate::fs::proc::devices::devices_node;
use crate::fs::proc::kallsyms::kallsyms_node;
use crate::fs::proc::kmsg::kmsg_node;
use crate::fs::proc::loadavg::loadavg_node;
use crate::fs::proc::meminfo::meminfo_node;
use crate::fs::proc::misc::misc_node;
use crate::fs::proc::pid_directory::pid_directory;
use crate::fs::proc::pressure_directory::pressure_directory;
use crate::fs::proc::self_symlink::self_node;
use crate::fs::proc::stat::stat_node;
use crate::fs::proc::swaps::swaps_node;
use crate::fs::proc::sysctl::{net_directory, sysctl_directory};
use crate::fs::proc::sysrq::sysrq_node;
use crate::fs::proc::thread_self::thread_self_node;
use crate::fs::proc::uid_cputime::uid_cputime_node;
use crate::fs::proc::uid_io::uid_io_node;
use crate::fs::proc::uid_procstat::uid_procstat_node;
use crate::fs::proc::uptime::uptime_node;
use crate::fs::proc::vmstat::vmstat_node;
use crate::mm::PAGE_SIZE;
use crate::task::{CurrentTask, KernelStats};
use crate::vfs::{
    emit_dotdot, fileops_impl_directory, fileops_impl_noop_sync, fs_node_impl_dir_readonly,
    fs_node_impl_symlink, unbounded_seek, BytesFile, DirectoryEntryType, DirentSink, DynamicFile,
    DynamicFileBuf, DynamicFileSource, FileObject, FileOps, FileSystemHandle, FsNode, FsNodeHandle,
    FsNodeInfo, FsNodeOps, FsStr, FsString, SeekTarget, StubEmptyFile, SymlinkTarget,
};

use maplit::btreemap;
use starnix_logging::{bug_ref, log_error, BugRef};
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::auth::FsCred;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::version::{KERNEL_RELEASE, KERNEL_VERSION};
use starnix_uapi::{errno, off_t, pid_t};
use std::collections::BTreeMap;
use std::sync::Arc;

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
            "cpuinfo".into() => cpuinfo_node(current_task, fs),
            "cmdline".into() => cmdline_node(current_task, fs),
            "devices".into() => devices_node(current_task, fs),
            "device-tree".into() => device_tree_node(current_task, fs),
            "self".into() => self_node(current_task, fs),
            "thread-self".into() => thread_self_node(current_task, fs),
            "meminfo".into() => meminfo_node(current_task, fs),
            "kmsg".into() => kmsg_node(current_task, fs),
            "kallsyms".into() => kallsyms_node(current_task, fs),
            "mounts".into() => MountsSymlink::new_node(current_task, fs),
            "cgroups".into() => cgroups_node(current_task, fs),
            "stat".into() => stat_node(current_task, fs),
            "swaps".into() => swaps_node(current_task, fs),
            "sys".into() => sysctl_directory(current_task, fs),
            "net".into() => net_directory(current_task, fs),
            "uptime".into() => uptime_node(current_task, fs),
            "loadavg".into() => loadavg_node(current_task, fs),
            "config.gz".into() => config_gz_node(current_task, fs),
            "sysrq-trigger".into() => sysrq_node(current_task, fs),
            "asound".into() => stub_node(current_task, fs, "/proc/asound", bug_ref!("https://fxbug.dev/322893329")),
            "diskstats".into() => stub_node(current_task, fs, "/proc/diskstats", bug_ref!("https://fxbug.dev/322893370")),
            "filesystems".into() => bytes_node(current_task, fs, b"fxfs".to_vec()),
            "misc".into() => misc_node(current_task, fs),
            // Starnix does not support dynamically loading modules.
            // Instead, we pretend to have loaded a single module, ferris (named after
            // Rust's 🦀), to avoid breaking code that assumes the modules list is
            // non-empty.
            "modules".into() => bytes_node(current_task, fs, b"ferris 8192 0 - Live 0x0000000000000000\n".to_vec()),
            "pagetypeinfo".into() => stub_node(current_task, fs, "/proc/pagetypeinfo", bug_ref!("https://fxbug.dev/322894315")),
            "slabinfo".into() => stub_node(current_task, fs, "/proc/slabinfo", bug_ref!("https://fxbug.dev/322894195")),
            "uid_cputime".into() => uid_cputime_node(current_task, fs),
            "uid_io".into() => uid_io_node(current_task, fs),
            "uid_procstat".into() => uid_procstat_node(current_task, fs),
            "version".into() => {
                let release = KERNEL_RELEASE;
                let user = "build-user@build-host";
                let toolchain = "clang version HEAD, LLD HEAD";
                let version = KERNEL_VERSION;
                let version_string = format!("Linux version {} ({}) ({}) {}\n", release, user, toolchain, version);
                bytes_node(current_task, fs, version_string.into())
            },
            "vmallocinfo".into() => stub_node(current_task, fs, "/proc/vmallocinfo", bug_ref!("https://fxbug.dev/322894183")),
            "vmstat".into() => vmstat_node(current_task, fs),
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

fn stub_node(
    current_task: &CurrentTask,
    fs: &FileSystemHandle,
    name: &'static str,
    bug: BugRef,
) -> FsNodeHandle {
    fs.create_node(
        current_task,
        StubEmptyFile::new_node(name, bug),
        FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
    )
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

/// Returns a new `BytesFile` containing the provided `bytes`.
fn bytes_node(current_task: &CurrentTask, fs: &FileSystemHandle, bytes: Vec<u8>) -> FsNodeHandle {
    fs.create_node(
        current_task,
        BytesFile::new_node(bytes),
        FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
    )
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
