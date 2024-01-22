// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    mm::PAGE_SIZE,
    task::{
        ptrace_get_scope, ptrace_set_scope, CurrentTask, NetstackDevicesDirectory, SeccompAction,
    },
    vfs::{
        fs_args, inotify, inotify::InotifyLimits, BytesFile, BytesFileOps, FileSystemHandle,
        FsNodeHandle, FsNodeInfo, FsNodeOps, FsString, StaticDirectoryBuilder,
    },
};
use starnix_sync::Mutex;
use starnix_uapi::{
    auth::{FsCred, CAP_SYS_ADMIN, CAP_SYS_RESOURCE},
    error,
    errors::Errno,
    file_mode::mode,
};
use std::{
    borrow::Cow,
    sync::atomic::{AtomicI32, AtomicUsize, Ordering},
};

pub fn sysctl_directory(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
    let mode = mode!(IFREG, 0o644);
    let mut dir = StaticDirectoryBuilder::new(fs);
    dir.subdir(current_task, "kernel", 0o555, |dir| {
        dir.entry(current_task, "unprivileged_bpf_disable", StubSysctl::new_node(), mode);
        dir.entry(current_task, "kptr_restrict", StubSysctl::new_node(), mode);
        dir.node(
            "overflowuid",
            fs.create_node(
                current_task,
                BytesFile::new_node(b"65534".to_vec()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o644), FsCred::root()),
            ),
        );
        dir.node(
            "overflowgid",
            fs.create_node(
                current_task,
                BytesFile::new_node(b"65534".to_vec()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o644), FsCred::root()),
            ),
        );
        dir.node(
            "pid_max",
            fs.create_node(
                current_task,
                BytesFile::new_node(b"4194304".to_vec()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o644), FsCred::root()),
            ),
        );
        dir.subdir(current_task, "random", 0o555, |dir| {
            dir.entry(
                current_task,
                "entropy_avail",
                BytesFile::new_node(b"256".to_vec()),
                mode!(IFREG, 0o444),
            );
            dir.entry(current_task, "actions_logged", SeccompActionsLogged::new_node(), mode);
        });
        dir.entry(current_task, "tainted", KernelTaintedFile::new_node(), mode);
        dir.subdir(current_task, "seccomp", 0o555, |dir| {
            dir.entry(
                current_task,
                "actions_avail",
                BytesFile::new_node(SeccompAction::get_actions_avail_file()),
                mode!(IFREG, 0o444),
            );
            dir.entry(current_task, "actions_logged", SeccompActionsLogged::new_node(), mode);
        });
        dir.subdir(current_task, "yama", 0o555, |dir| {
            dir.entry(current_task, "ptrace_scope", PtraceYamaScope::new_node(), mode);
        });
    });
    dir.node("net", sysctl_net_diretory(current_task, fs));
    dir.subdir(current_task, "vm", 0o555, |dir| {
        dir.entry(current_task, "mmap_rnd_bits", StubSysctl::new_node(), mode);
        dir.entry(current_task, "mmap_rnd_compat_bits", StubSysctl::new_node(), mode);
        dir.entry(current_task, "drop_caches", StubSysctl::new_node(), mode);
    });
    dir.subdir(current_task, "fs", 0o555, |dir| {
        dir.subdir(current_task, "inotify", 0o555, |dir| {
            dir.entry(
                current_task,
                "max_queued_events",
                inotify::InotifyMaxQueuedEvents::new_node(),
                mode,
            );
            dir.entry(
                current_task,
                "max_user_instances",
                inotify::InotifyMaxUserInstances::new_node(),
                mode,
            );
            dir.entry(
                current_task,
                "max_user_watches",
                inotify::InotifyMaxUserWatches::new_node(),
                mode,
            );
        });
        dir.entry(current_task, "pipe-max-size", PipeMaxSizeFile::new_node(), mode);
    });
    dir.build(current_task)
}

pub struct SystemLimits {
    /// Limits applied to inotify objects.
    pub inotify: InotifyLimits,

    /// The maximum size of pipes in the system.
    pub pipe_max_size: AtomicUsize,
}

impl Default for SystemLimits {
    fn default() -> SystemLimits {
        SystemLimits {
            inotify: InotifyLimits {
                max_queued_events: AtomicI32::new(16384),
                max_user_instances: AtomicI32::new(128),
                max_user_watches: AtomicI32::new(1048576),
            },
            pipe_max_size: AtomicUsize::new((*PAGE_SIZE * 256) as usize),
        }
    }
}

struct StubSysctl {
    data: Mutex<Vec<u8>>,
}

impl StubSysctl {
    fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self { data: Mutex::default() })
    }
}

impl BytesFileOps for StubSysctl {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        *self.data.lock() = data;
        Ok(())
    }
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(self.data.lock().clone().into())
    }
}

pub fn net_directory(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
    let mut dir = StaticDirectoryBuilder::new(fs);
    dir.subdir(current_task, "xt_quota", 0o555, |dir| {
        dir.entry(current_task, "globalAlert", StubSysctl::new_node(), mode!(IFREG, 0o444));
    });
    dir.build(current_task)
}

struct KernelTaintedFile;

impl KernelTaintedFile {
    fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self)
    }
}

impl BytesFileOps for KernelTaintedFile {
    fn write(&self, _current_task: &CurrentTask, _data: Vec<u8>) -> Result<(), Errno> {
        Ok(())
    }
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(Cow::Borrowed(&b"0\n"[..]))
    }
}

/// Holds the device-specific directories that are found under `/proc/sys/net`.
pub struct ProcSysNetDev {
    /// The `/proc/sys/net/ipv4/{device}/neigh` directory.
    ipv4_neigh: FsNodeHandle,
    /// The `/proc/sys/net/ipv6/{device}/conf` directory.
    ipv6_conf: FsNodeHandle,
    /// The `/proc/sys/net/ipv6/{device}/neigh` directory.
    ipv6_neigh: FsNodeHandle,
}

impl ProcSysNetDev {
    pub fn new(current_task: &CurrentTask, proc_fs: &FileSystemHandle) -> Self {
        let file_mode = mode!(IFREG, 0o644);
        // TODO(https://fxbug.dev/128995): Implement the "files" properly instead
        // of using stubs.
        ProcSysNetDev {
            ipv4_neigh: {
                let mut dir = StaticDirectoryBuilder::new(proc_fs);
                dir.entry(current_task, "ucast_solicit", StubSysctl::new_node(), file_mode);
                dir.entry(current_task, "retrans_time_ms", StubSysctl::new_node(), file_mode);
                dir.entry(current_task, "mcast_resolicit", StubSysctl::new_node(), file_mode);
                dir.build(current_task)
            },
            ipv6_conf: {
                let mut dir = StaticDirectoryBuilder::new(proc_fs);
                dir.entry(current_task, "accept_ra", StubSysctl::new_node(), file_mode);
                dir.entry(current_task, "dad_transmits", StubSysctl::new_node(), file_mode);
                dir.entry(current_task, "use_tempaddr", StubSysctl::new_node(), file_mode);
                dir.entry(current_task, "addr_gen_mode", StubSysctl::new_node(), file_mode);
                dir.entry(current_task, "stable_secret", StubSysctl::new_node(), file_mode);
                dir.entry(current_task, "disable_ipv6", StubSysctl::new_node(), file_mode);
                dir.build(current_task)
            },
            ipv6_neigh: {
                let mut dir = StaticDirectoryBuilder::new(proc_fs);
                dir.entry(current_task, "ucast_solicit", StubSysctl::new_node(), file_mode);
                dir.entry(current_task, "retrans_time_ms", StubSysctl::new_node(), file_mode);
                dir.entry(current_task, "mcast_resolicit", StubSysctl::new_node(), file_mode);
                dir.build(current_task)
            },
        }
    }

    pub fn get_ipv4_neigh(&self) -> &FsNodeHandle {
        &self.ipv4_neigh
    }

    pub fn get_ipv6_conf(&self) -> &FsNodeHandle {
        &self.ipv6_conf
    }

    pub fn get_ipv6_neigh(&self) -> &FsNodeHandle {
        &self.ipv6_neigh
    }
}

fn sysctl_net_diretory(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
    let mut dir = StaticDirectoryBuilder::new(fs);
    let file_mode = mode!(IFREG, 0o644);
    let dir_mode = mode!(IFDIR, 0o644);

    let devs = &current_task.kernel().netstack_devices;
    // Per https://www.kernel.org/doc/Documentation/networking/ip-sysctl.txt,
    //
    //   conf/default/*:
    //	   Change the interface-specific default settings.
    //
    //   conf/all/*:
    //	   Change all the interface-specific settings.
    //
    // Note that the all/default directories don't exist in `/sys/class/net`.
    devs.add_dev(current_task, "all", Some(fs), None /* sys_fs */);
    devs.add_dev(current_task, "default", Some(fs), None /* sys_fs */);

    dir.subdir(current_task, "core", 0o555, |dir| {
        dir.entry(current_task, "bpf_jit_enable", StubSysctl::new_node(), file_mode);
        dir.entry(current_task, "bpf_jit_kallsyms", StubSysctl::new_node(), file_mode);
    });
    dir.subdir(current_task, "ipv4", 0o555, |dir| {
        dir.entry(
            current_task,
            "neigh",
            NetstackDevicesDirectory::new_proc_sys_net_ipv4_neigh(),
            dir_mode,
        );
    });
    dir.subdir(current_task, "ipv6", 0o555, |dir| {
        dir.entry(
            current_task,
            "conf",
            NetstackDevicesDirectory::new_proc_sys_net_ipv6_conf(),
            dir_mode,
        );
        dir.entry(
            current_task,
            "neigh",
            NetstackDevicesDirectory::new_proc_sys_net_ipv6_neigh(),
            dir_mode,
        );
    });
    dir.build(current_task)
}

struct SeccompActionsLogged {}

impl SeccompActionsLogged {
    fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self {})
    }
}

impl BytesFileOps for SeccompActionsLogged {
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        if !current_task.creds().has_capability(CAP_SYS_ADMIN) {
            return error!(EPERM);
        }
        SeccompAction::set_actions_logged(current_task.kernel(), &data)?;
        Ok(())
    }
    fn read(&self, current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(SeccompAction::get_actions_logged(current_task.kernel()).into())
    }
}

struct PtraceYamaScope {}

impl PtraceYamaScope {
    fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self {})
    }
}

impl BytesFileOps for PtraceYamaScope {
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        if !current_task.creds().has_capability(CAP_SYS_ADMIN) {
            return error!(EPERM);
        }
        ptrace_set_scope(current_task.kernel(), &data)?;
        Ok(())
    }
    fn read(&self, current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(ptrace_get_scope(current_task.kernel()).into())
    }
}

trait AtomicGetter {
    fn get_atomic<'a>(current_task: &'a CurrentTask) -> &'a AtomicUsize;
}

struct PipeMaxSizeGetter;
impl AtomicGetter for PipeMaxSizeGetter {
    fn get_atomic<'a>(current_task: &'a CurrentTask) -> &'a AtomicUsize {
        &current_task.kernel().system_limits.pipe_max_size
    }
}

struct SystemLimitFile<G: AtomicGetter + Send + Sync + 'static> {
    marker: std::marker::PhantomData<G>,
}

impl<G: AtomicGetter + Send + Sync + 'static> SystemLimitFile<G> {
    pub fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self { marker: Default::default() })
    }
}

impl<G: AtomicGetter + Send + Sync + 'static> BytesFileOps for SystemLimitFile<G> {
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        if !current_task.creds().has_capability(CAP_SYS_RESOURCE) {
            return error!(EPERM);
        }
        let value = fs_args::parse::<usize>(FsString::from(data).as_ref())?;
        G::get_atomic(current_task).store(value, Ordering::Relaxed);
        Ok(())
    }

    fn read(&self, current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(G::get_atomic(current_task).load(Ordering::Relaxed).to_string().into_bytes().into())
    }
}

type PipeMaxSizeFile = SystemLimitFile<PipeMaxSizeGetter>;
