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
        FsNodeHandle, FsNodeInfo, FsNodeOps, FsString, SimpleFileNode, StaticDirectoryBuilder,
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
        dir.entry(
            current_task,
            "unprivileged_bpf_disable",
            StubSysctl::new_node("/proc/sys/kernel/unprivileged_bpf_disable", None),
            mode,
        );
        dir.entry(
            current_task,
            "kptr_restrict",
            StubSysctl::new_node("/proc/sys/kernel/kptr_restrict", None),
            mode,
        );
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
        dir.entry(
            current_task,
            "dirty_background_ratio",
            StubSysctl::new_node("/proc/sys/vm/dirty_background_ratio", None),
            mode,
        );
        dir.entry(
            current_task,
            "dirty_expire_centisecs",
            StubSysctl::new_node("/proc/sys/vm/dirty_expire_centisecs", None),
            mode,
        );
        dir.entry(
            current_task,
            "drop_caches",
            StubSysctl::new_node("/proc/sys/vm/drop_caches", None),
            mode,
        );
        dir.entry(
            current_task,
            "extra_free_kbytes",
            StubSysctl::new_node("/proc/sys/vm/extra_free_kbytes", None),
            mode,
        );
        dir.entry(
            current_task,
            "max_map_count",
            StubSysctl::new_node("/proc/sys/vm/max_map_count", None),
            mode,
        );
        dir.entry(
            current_task,
            "mmap_min_addr",
            StubSysctl::new_node("/proc/sys/vm/mmap_min_addr", None),
            mode,
        );
        dir.entry(
            current_task,
            "mmap_rnd_bits",
            StubSysctl::new_node("/proc/sys/vm/mmap_rnd_bits", None),
            mode,
        );
        dir.entry(
            current_task,
            "mmap_rnd_compat_bits",
            StubSysctl::new_node("/proc/sys/vm/mmap_rnd_compat_bits", None),
            mode,
        );
        dir.entry(
            current_task,
            "overcommit_memory",
            StubSysctl::new_node("/proc/sys/vm/overcommit_memory", None),
            mode,
        );
        dir.entry(
            current_task,
            "page-cluster",
            StubSysctl::new_node("/proc/sys/vm/page-cluster", None),
            mode,
        );
        dir.entry(
            current_task,
            "watermark_scale_factor",
            StubSysctl::new_node("/proc/sys/vm/watermark_scale_factor", None),
            mode,
        );
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
        dir.entry(
            current_task,
            "protected_hardlinks",
            StubSysctl::new_node("/proc/sys/fs/protected_hardlinks", None),
            mode,
        );
        dir.entry(
            current_task,
            "protected_symlinks",
            StubSysctl::new_node("/proc/sys/fs/protected_symlinks", None),
            mode,
        );
        dir.entry(
            current_task,
            "suid_dumpable",
            StubSysctl::new_node("/proc/sys/fs/suid_dumpable", None),
            mode,
        );
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

#[derive(Default)]
struct StubSysctl {
    data: Mutex<Vec<u8>>,
}

impl StubSysctl {
    #[track_caller]
    fn new_node(message: &'static str, bug_url: Option<&'static str>) -> impl FsNodeOps {
        let location = std::panic::Location::caller();
        let file = BytesFile::new(Self::default());
        SimpleFileNode::new(move || {
            starnix_logging::__track_stub_inner(bug_url, message, None, location);
            Ok(file.clone())
        })
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
    dir.entry(
        current_task,
        "fib_trie",
        StubSysctl::new_node("/proc/net/fib_trie", None),
        mode!(IFREG, 0o400),
    );
    dir.entry(
        current_task,
        "if_inet6",
        StubSysctl::new_node("/proc/net/if_inet6", None),
        mode!(IFREG, 0o444),
    );
    dir.entry(
        current_task,
        "psched",
        StubSysctl::new_node("/proc/net/psched", None),
        mode!(IFREG, 0o444),
    );
    dir.entry(
        current_task,
        "xt_qtaguid",
        StubSysctl::new_node("/proc/net/xt_qtaguid", None),
        mode!(IFREG, 0o644),
    );
    dir.subdir(current_task, "xt_quota", 0o555, |dir| {
        dir.entry(
            current_task,
            "globalAlert",
            StubSysctl::new_node("/proc/net/xt_quota/globalAlert", None),
            mode!(IFREG, 0o444),
        );
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
    /// The `/proc/sys/net/ipv4/{device}/conf` directory.
    ipv4_conf: FsNodeHandle,
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
        ProcSysNetDev {
            ipv4_conf: {
                let mut dir = StaticDirectoryBuilder::new(proc_fs);
                dir.entry(
                    current_task,
                    "accept_redirects",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv4/DEVICE/conf/accept_redirects",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.build(current_task)
            },
            ipv4_neigh: {
                let mut dir = StaticDirectoryBuilder::new(proc_fs);
                dir.entry(
                    current_task,
                    "ucast_solicit",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv4/DEVICE/neigh/ucast_solicit",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "retrans_time_ms",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv4/DEVICE/neigh/retrans_time_ms",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "mcast_resolicit",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv4/DEVICE/neigh/mcast_resolicit",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "base_reachable_time_ms",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv4/DEVICE/neigh/base_reachable_time_ms",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.build(current_task)
            },
            ipv6_conf: {
                let mut dir = StaticDirectoryBuilder::new(proc_fs);
                dir.entry(
                    current_task,
                    "accept_ra",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/accept_ra",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "accept_ra_info_min_plen",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/accept_ra_info_min_plen",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "accept_ra_rt_table",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/accept_ra_rt_table",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "accept_redirects",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/accept_redirects",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "dad_transmits",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/dad_transmits",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "use_tempaddr",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/use_tempaddr",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "addr_gen_mode",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/addr_gen_mode",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "stable_secret",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/stable_secret",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "disable_ipv6",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/disable_ip64",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "optimistic_dad",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/optimistic_dad",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "use_oif_addrs_only",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/use_oif_addrs_only",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "use_optimistic",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/use_optimistic",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.build(current_task)
            },
            ipv6_neigh: {
                let mut dir = StaticDirectoryBuilder::new(proc_fs);
                dir.entry(
                    current_task,
                    "ucast_solicit",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/neigh/ucast_solicit",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "retrans_time_ms",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/neigh/retrans_time_ms",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "mcast_resolicit",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/neigh/mcast_resolicit",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "base_reachable_time_ms",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/neigh/base_reachable_time_ms",
                        Some("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.build(current_task)
            },
        }
    }

    pub fn get_ipv4_conf(&self) -> &FsNodeHandle {
        &self.ipv4_conf
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
        dir.entry(
            current_task,
            "bpf_jit_enable",
            StubSysctl::new_node("/proc/sys/net/bpf_jit_enable", None),
            file_mode,
        );
        dir.entry(
            current_task,
            "bpf_jit_kallsyms",
            StubSysctl::new_node("/proc/sys/net/bpf_jit_kallsyms", None),
            file_mode,
        );
    });
    dir.subdir(current_task, "ipv4", 0o555, |dir| {
        dir.entry(
            current_task,
            "conf",
            NetstackDevicesDirectory::new_proc_sys_net_ipv4_conf(),
            dir_mode,
        );
        dir.entry(
            current_task,
            "fwmark_reflect",
            StubSysctl::new_node("/proc/sys/net/ipv4/fwmark_reflect", None),
            file_mode,
        );
        dir.entry(
            current_task,
            "ip_forward",
            StubSysctl::new_node("/proc/sys/net/ipv4/ip_forward", None),
            file_mode,
        );
        dir.entry(
            current_task,
            "neigh",
            NetstackDevicesDirectory::new_proc_sys_net_ipv4_neigh(),
            dir_mode,
        );
        dir.entry(
            current_task,
            "ping_group_range",
            StubSysctl::new_node("/proc/sys/net/ipv4/ping_group_range", None),
            file_mode,
        );
        dir.entry(
            current_task,
            "tcp_default_init_rwnd",
            StubSysctl::new_node("/proc/sys/net/ipv4/tcp_default_init_rwnd", None),
            file_mode,
        );
        dir.entry(
            current_task,
            "tcp_fwmark_accept",
            StubSysctl::new_node("/proc/sys/net/ipv4/tcp_fwmark_accept", None),
            file_mode,
        );
        dir.entry(
            current_task,
            "tcp_rmem",
            StubSysctl::new_node("/proc/sys/net/ipv4/tcp_rmem", None),
            file_mode,
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
            "fwmark_reflect",
            StubSysctl::new_node("/proc/sys/net/ipv6/fwmark_reflect", None),
            file_mode,
        );
        dir.entry(
            current_task,
            "neigh",
            NetstackDevicesDirectory::new_proc_sys_net_ipv6_neigh(),
            dir_mode,
        );
    });
    dir.subdir(current_task, "unix", 0o555, |dir| {
        dir.entry(
            current_task,
            "max_dgram_qlen",
            StubSysctl::new_node("/proc/sys/net/unix/max_dgram_qlen", None),
            file_mode,
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
