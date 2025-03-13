// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fs::proc::cgroups::cgroups_file;
use crate::fs::proc::config_gz::ConfigFile;
use crate::fs::proc::cpuinfo::CpuinfoFile;
use crate::fs::proc::device_tree::device_tree_directory;
use crate::fs::proc::devices::DevicesFile;
use crate::fs::proc::kmsg::kmsg_file;
use crate::fs::proc::loadavg::LoadavgFile;
use crate::fs::proc::meminfo::MeminfoFile;
use crate::fs::proc::misc::MiscFile;
use crate::fs::proc::mounts_symlink::MountsSymlink;
use crate::fs::proc::pid_directory::pid_directory;
use crate::fs::proc::pressure_directory::pressure_directory;
use crate::fs::proc::self_symlink::SelfSymlink;
use crate::fs::proc::stat::StatFile;
use crate::fs::proc::swaps::SwapsFile;
use crate::fs::proc::sysctl::{net_directory, sysctl_directory};
use crate::fs::proc::sysrq::SysRqNode;
use crate::fs::proc::thread_self::ThreadSelfSymlink;
use crate::fs::proc::uid_cputime::uid_cputime_directory;
use crate::fs::proc::uid_io::uid_io_directory;
use crate::fs::proc::uid_procstat::uid_procstat_directory;
use crate::fs::proc::uptime::UptimeFile;
use crate::fs::proc::vmstat::VmStatFile;
use crate::fs::proc::zoneinfo::ZoneInfoFile;
use crate::task::CurrentTask;
use crate::vfs::{
    emit_dotdot, fileops_impl_directory, fileops_impl_noop_sync, fs_node_impl_dir_readonly,
    unbounded_seek, BytesFile, DirectoryEntryType, DirentSink, FileObject, FileOps,
    FileSystemHandle, FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr, FsString, SeekTarget,
    SimpleFileNode, StubEmptyFile,
};

use maplit::btreemap;
use starnix_logging::{bug_ref, track_stub, BugRef};
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
        // First add all the nodes that are always present in the top-level proc directory.
        let mut nodes = btreemap! {
            "asound".into() => stub_file(current_task, fs, "/proc/asound", bug_ref!("https://fxbug.dev/322893329")),
            "cgroups".into() => cgroups_file(current_task, fs),
            "cmdline".into() => {
                let mut cmdline = Vec::from(current_task.kernel().cmdline.clone());
                cmdline.push(b'\n');
                read_only_file(current_task, fs, BytesFile::new_node(cmdline))
            },
            "config.gz".into() => read_only_file(current_task, fs, ConfigFile::new_node()),
            "cpuinfo".into() => read_only_file(current_task, fs, CpuinfoFile::new_node()),
            "devices".into() => read_only_file(current_task, fs, DevicesFile::new_node()),
            "device-tree".into() => device_tree_directory(current_task, fs),
            "diskstats".into() => stub_file(current_task, fs, "/proc/diskstats", bug_ref!("https://fxbug.dev/322893370")),
            "filesystems".into() => bytes_file(current_task, fs, b"fxfs".to_vec()),
            "kallsyms".into() => read_only_file(current_task, fs, SimpleFileNode::new(|| {
                track_stub!(TODO("https://fxbug.dev/369067922"), "Provide a real /proc/kallsyms");
                Ok(BytesFile::new(b"0000000000000000 T security_inode_copy_up".to_vec()))
            })),
            "kmsg".into() => kmsg_file(current_task, fs),
            "loadavg".into() => read_only_file(current_task, fs, LoadavgFile::new_node(kernel)),
            "meminfo".into() => read_only_file(current_task, fs, MeminfoFile::new_node(&kernel.stats)),
            "misc".into() => read_only_file(current_task, fs, MiscFile::new_node()),
            // Starnix does not support dynamically loading modules.
            // Instead, we pretend to have loaded a single module, ferris (named after
            // Rust's 🦀), to avoid breaking code that assumes the modules list is
            // non-empty.
            "modules".into() => bytes_file(current_task, fs, b"ferris 8192 0 - Live 0x0000000000000000\n".to_vec()),
            "mounts".into() => symlink_file(current_task, fs, MountsSymlink::new_node()),
            "net".into() => net_directory(current_task, fs),
            "pagetypeinfo".into() => stub_file(current_task, fs, "/proc/pagetypeinfo", bug_ref!("https://fxbug.dev/322894315")),
            "self".into() => symlink_file(current_task, fs, SelfSymlink::new_node()),
            "slabinfo".into() => stub_file(current_task, fs, "/proc/slabinfo", bug_ref!("https://fxbug.dev/322894195")),
            "stat".into() => read_only_file(current_task, fs, StatFile::new_node(&kernel.stats)),
            "swaps".into() => read_only_file(current_task, fs, SwapsFile::new_node()),
            "sys".into() => sysctl_directory(current_task, fs),
            "sysrq-trigger".into() => root_writable_file(current_task, fs, SysRqNode::new()),
            "thread-self".into() => symlink_file(current_task, fs, ThreadSelfSymlink::new_node()),
            "uid_cputime".into() => uid_cputime_directory(current_task, fs),
            "uid_io".into() => uid_io_directory(current_task, fs),
            "uid_procstat".into() => uid_procstat_directory(current_task, fs),
            "uptime".into() => read_only_file(current_task, fs, UptimeFile::new_node(&kernel.stats)),
            "version".into() => {
                let release = KERNEL_RELEASE;
                let user = "build-user@build-host";
                let toolchain = "clang version HEAD, LLD HEAD";
                let version = KERNEL_VERSION;
                let version_string = format!("Linux version {} ({}) ({}) {}\n", release, user, toolchain, version);
                bytes_file(current_task, fs, version_string.into())
            },
            "vmallocinfo".into() => stub_file(current_task, fs, "/proc/vmallocinfo", bug_ref!("https://fxbug.dev/322894183")),
            "vmstat".into() => read_only_file(current_task, fs, VmStatFile::new_node(&kernel.stats)),
            "zoneinfo".into() => read_only_file(current_task, fs, ZoneInfoFile::new_node(&kernel.stats)),
        };

        // Then optionally add the nodes that are only present in some configurations.
        if let Some(pressure_directory) = pressure_directory(current_task, fs) {
            nodes.insert("pressure".into(), pressure_directory);
        }

        Arc::new(ProcDirectory { nodes })
    }
}

/// Creates a stub file that logs a message with the associated bug when it is accessed.
fn stub_file(
    current_task: &CurrentTask,
    fs: &FileSystemHandle,
    name: &'static str,
    bug: BugRef,
) -> FsNodeHandle {
    read_only_file(current_task, fs, StubEmptyFile::new_node(name, bug))
}

/// Returns a new `BytesFile` containing the provided `bytes`.
fn bytes_file(current_task: &CurrentTask, fs: &FileSystemHandle, bytes: Vec<u8>) -> FsNodeHandle {
    read_only_file(current_task, fs, BytesFile::new_node(bytes))
}

/// Creates a standard read-only file suitable for use in `ProcDirectory`.
fn read_only_file(
    current_task: &CurrentTask,
    fs: &FileSystemHandle,
    ops: impl Into<Box<dyn FsNodeOps>>,
) -> FsNodeHandle {
    fs.create_node(current_task, ops, FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()))
}

/// Creates a file that is only writable by root.
fn root_writable_file(
    current_task: &CurrentTask,
    fs: &FileSystemHandle,
    ops: impl Into<Box<dyn FsNodeOps>>,
) -> FsNodeHandle {
    fs.create_node(current_task, ops, FsNodeInfo::new_factory(mode!(IFREG, 0o200), FsCred::root()))
}

fn symlink_file(
    current_task: &CurrentTask,
    fs: &FileSystemHandle,
    ops: impl Into<Box<dyn FsNodeOps>>,
) -> FsNodeHandle {
    fs.create_node(current_task, ops, FsNodeInfo::new_factory(mode!(IFLNK, 0o777), FsCred::root()))
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
