// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::mm::PAGE_SIZE;
use crate::task::{
    ptrace_get_scope, ptrace_set_scope, CurrentTask, NetstackDevicesDirectory, SeccompAction,
};
use crate::vfs::inotify::InotifyLimits;
use crate::vfs::{
    fs_args, inotify, BytesFile, BytesFileOps, FileSystemHandle, FsNodeHandle, FsNodeInfo,
    FsNodeOps, FsString, SimpleFileNode, StaticDirectoryBuilder,
};
use starnix_logging::bug_ref;
use starnix_sync::Mutex;
use starnix_uapi::auth::{
    Capabilities, FsCred, CAP_LAST_CAP, CAP_NET_ADMIN, CAP_SYS_ADMIN, CAP_SYS_RESOURCE,
};
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::version::{KERNEL_RELEASE, KERNEL_VERSION};
use starnix_uapi::{errno, error};
use std::borrow::Cow;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use uuid::Uuid;

pub fn sysctl_directory(current_task: &CurrentTask, fs: &FileSystemHandle) -> FsNodeHandle {
    let mode = mode!(IFREG, 0o644);
    let mut dir = StaticDirectoryBuilder::new(fs);
    dir.subdir(current_task, "abi", 0o555, |_dir| {
        #[cfg(target_arch = "aarch64")]
        _dir.entry(
            current_task,
            "swp",
            StubSysctl::new_node("/proc/sys/abi/swp", bug_ref!("https://fxbug.dev/322873460")),
            mode,
        );
    });
    dir.subdir(current_task, "kernel", 0o555, |dir| {
        dir.node(
            "cap_last_cap",
            fs.create_node(
                current_task,
                BytesFile::new_node(|| Ok(format!("{}\n", CAP_LAST_CAP))),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
        );
        dir.node(
            "core_pattern",
            fs.create_node(
                current_task,
                // TODO(https://fxbug.dev/322873960): Use the core pattern when generating a core dump.
                BytesFile::new_node(b"core".to_vec()),
                FsNodeInfo::new_factory(mode, FsCred::root()),
            ),
        );
        dir.entry(
            current_task,
            "core_pipe_limit",
            StubSysctl::new_node(
                "/proc/sys/kernel/core_pipe_limit",
                bug_ref!("https://fxbug.dev/322873721"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "dmsg_restrict",
            StubSysctl::new_node(
                "/proc/sys/kernel/dmsg_restrict",
                bug_ref!("https://fxbug.dev/322874424"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "domainname",
            StubSysctl::new_node(
                "/proc/sys/kernel/domainname",
                bug_ref!("https://fxbug.dev/322873722"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "hostname",
            StubSysctl::new_node(
                "/proc/sys/kernel/hostname",
                bug_ref!("https://fxbug.dev/322873462"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "hung_task_check_count",
            StubSysctl::new_node(
                "/proc/sys/kernel/hung_task_check_count",
                bug_ref!("https://fxbug.dev/322874644"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "hung_task_panic",
            StubSysctl::new_node(
                "/proc/sys/kernel/hung_task_panic",
                bug_ref!("https://fxbug.dev/322874332"),
            ),
            mode,
        );
        //
        dir.entry(
            current_task,
            "hung_task_timeout_secs",
            StubSysctl::new_node(
                "/proc/sys/kernel/hung_task_timeout_secs",
                bug_ref!("https://fxbug.dev/322873962"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "hung_task_warnings",
            StubSysctl::new_node(
                "/proc/sys/kernel/hung_task_warnings",
                bug_ref!("https://fxbug.dev/322873740"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "io_uring_disabled",
            SystemLimitFile::<IoUringDisabled>::new_node(),
            mode,
        );
        dir.entry(
            current_task,
            "io_uring_group",
            SystemLimitFile::<IoUringGroup>::new_node(),
            mode,
        );
        dir.entry(
            current_task,
            "modprobe",
            StubSysctl::new_node(
                "/proc/sys/kernel/modprobe",
                bug_ref!("https://fxbug.dev/322874334"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "modules_disabled",
            StubSysctl::new_node(
                "/proc/sys/kernel/modules_disabled",
                bug_ref!("https://fxbug.dev/322874489"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "panic_on_oops",
            StubSysctl::new_node(
                "/proc/sys/kernel/panic_on_oops",
                bug_ref!("https://fxbug.dev/322874296"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "perf_cpu_time_max_percent",
            StubSysctl::new_node(
                "/proc/sys/kernel/perf_cpu_time_max_percent",
                bug_ref!("https://fxbug.dev/322873262"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "perf_event_max_sample_rate",
            StubSysctl::new_node(
                "/proc/sys/kernel/perf_event_max_sample_rate",
                bug_ref!("https://fxbug.dev/322874604"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "perf_event_mlock_kb",
            StubSysctl::new_node(
                "/proc/sys/kernel/perf_event_mlock_kb",
                bug_ref!("https://fxbug.dev/322873800"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "perf_event_paranoid",
            StubSysctl::new_node(
                "/proc/sys/kernel/perf_event_paranoid",
                bug_ref!("https://fxbug.dev/322873896"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "randomize_va_space",
            StubSysctl::new_node(
                "/proc/sys/kernel/randomize_va_space",
                bug_ref!("https://fxbug.dev/322873202"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "sched_child_runs_first",
            StubSysctl::new_node(
                "/proc/sys/kernel/sched_child_runs_first",
                bug_ref!("https://fxbug.dev/322874709"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "sched_latency_ns",
            StubSysctl::new_node(
                "/proc/sys/kernel/sched_latency_ns",
                bug_ref!("https://fxbug.dev/322874319"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "sched_rt_period_us",
            StubSysctl::new_node(
                "/proc/sys/kernel/sched_rt_period_us",
                bug_ref!("https://fxbug.dev/322874785"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "sched_rt_runtime_us",
            StubSysctl::new_node(
                "/proc/sys/kernel/sched_rt_runtime_us",
                bug_ref!("https://fxbug.dev/322874726"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "sched_schedstats",
            StubSysctl::new_node(
                "/proc/sys/kernel/sched_schedstats",
                bug_ref!("https://fxbug.dev/322874584"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "sched_tunable_scaling",
            StubSysctl::new_node(
                "/proc/sys/kernel/sched_tunable_scaling",
                bug_ref!("https://fxbug.dev/322874666"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "sched_wakeup_granularity_ns",
            StubSysctl::new_node(
                "/proc/sys/kernel/sched_wakeup_granularity_ns",
                bug_ref!("https://fxbug.dev/322874525"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "sysrq",
            StubSysctl::new_node("/proc/sys/kernel/sysrq", bug_ref!("https://fxbug.dev/322874375")),
            mode,
        );
        dir.entry(
            current_task,
            "unprivileged_bpf_disabled",
            StubSysctl::new_node(
                "/proc/sys/kernel/unprivileged_bpf_disabled",
                bug_ref!("https://fxbug.dev/322874504"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "kptr_restrict",
            StubSysctl::new_node(
                "/proc/sys/kernel/kptr_restrict",
                bug_ref!("https://fxbug.dev/322873878"),
            ),
            mode,
        );
        dir.node(
            "osrelease",
            fs.create_node(
                current_task,
                BytesFile::new_node(|| Ok(format!("{}\n", KERNEL_RELEASE))),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
        );
        dir.node(
            "ostype",
            fs.create_node(
                current_task,
                BytesFile::new_node(b"Linux\n".to_vec()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
            ),
        );
        dir.node(
            "overflowuid",
            fs.create_node(
                current_task,
                BytesFile::new_node(b"65534".to_vec()),
                FsNodeInfo::new_factory(mode, FsCred::root()),
            ),
        );
        dir.node(
            "overflowgid",
            fs.create_node(
                current_task,
                BytesFile::new_node(b"65534".to_vec()),
                FsNodeInfo::new_factory(mode, FsCred::root()),
            ),
        );
        dir.node(
            "printk",
            fs.create_node(
                current_task,
                BytesFile::new_node(b"4\t4\t1\t7\n".to_vec()),
                FsNodeInfo::new_factory(mode, FsCred::root()),
            ),
        );
        dir.node(
            "pid_max",
            fs.create_node(
                current_task,
                BytesFile::new_node(b"4194304".to_vec()),
                FsNodeInfo::new_factory(mode, FsCred::root()),
            ),
        );
        dir.subdir(current_task, "random", 0o555, |dir| {
            // Generate random UUID
            let boot_id = Uuid::new_v4().hyphenated().to_string();
            dir.entry(
                current_task,
                "boot_id",
                BytesFile::new_node(boot_id.as_bytes().to_vec()),
                mode,
            );
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
    dir.node(
        "version",
        fs.create_node(
            current_task,
            BytesFile::new_node(|| Ok(format!("{}\n", KERNEL_VERSION))),
            FsNodeInfo::new_factory(mode!(IFREG, 0o444), FsCred::root()),
        ),
    );
    dir.subdir(current_task, "vm", 0o555, |dir| {
        dir.entry(
            current_task,
            "dirty_background_ratio",
            StubSysctl::new_node(
                "/proc/sys/vm/dirty_background_ratio",
                bug_ref!("https://fxbug.dev/322874492"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "dirty_expire_centisecs",
            StubSysctl::new_node(
                "/proc/sys/vm/dirty_expire_centisecs",
                bug_ref!("https://fxbug.dev/322874237"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "drop_caches",
            StubSysctl::new_node(
                "/proc/sys/vm/drop_caches",
                bug_ref!("https://fxbug.dev/322874299"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "extra_free_kbytes",
            StubSysctl::new_node(
                "/proc/sys/vm/extra_free_kbytes",
                bug_ref!("https://fxbug.dev/322873761"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "max_map_count",
            StubSysctl::new_node(
                "/proc/sys/vm/max_map_count",
                bug_ref!("https://fxbug.dev/322874684"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "mmap_min_addr",
            StubSysctl::new_node(
                "/proc/sys/vm/mmap_min_addr",
                bug_ref!("https://fxbug.dev/322874526"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "mmap_rnd_bits",
            StubSysctl::new_node(
                "/proc/sys/vm/mmap_rnd_bits",
                bug_ref!("https://fxbug.dev/322874505"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "mmap_rnd_compat_bits",
            StubSysctl::new_node(
                "/proc/sys/vm/mmap_rnd_compat_bits",
                bug_ref!("https://fxbug.dev/322874685"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "overcommit_memory",
            StubSysctl::new_node(
                "/proc/sys/vm/overcommit_memory",
                bug_ref!("https://fxbug.dev/322874159"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "page-cluster",
            StubSysctl::new_node(
                "/proc/sys/vm/page-cluster",
                bug_ref!("https://fxbug.dev/322874302"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "watermark_scale_factor",
            StubSysctl::new_node(
                "/proc/sys/vm/watermark_scale_factor",
                bug_ref!("https://fxbug.dev/322874321"),
            ),
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
        dir.entry(current_task, "pipe-max-size", SystemLimitFile::<PipeMaxSize>::new_node(), mode);
        dir.entry(
            current_task,
            "protected_hardlinks",
            StubSysctl::new_node(
                "/proc/sys/fs/protected_hardlinks",
                bug_ref!("https://fxbug.dev/322874347"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "protected_symlinks",
            StubSysctl::new_node(
                "/proc/sys/fs/protected_symlinks",
                bug_ref!("https://fxbug.dev/322874764"),
            ),
            mode,
        );
        dir.entry(
            current_task,
            "suid_dumpable",
            StubSysctl::new_node(
                "/proc/sys/fs/suid_dumpable",
                bug_ref!("https://fxbug.dev/322874210"),
            ),
            mode,
        );
        dir.subdir(current_task, "verity", 0o555, |dir| {
            dir.entry(
                current_task,
                "require_signatures",
                VerityRequireSignaturesFile::new_node(),
                mode,
            );
        });
    });
    dir.build(current_task)
}

struct VerityRequireSignaturesFile;

impl VerityRequireSignaturesFile {
    fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self)
    }
}

impl BytesFileOps for VerityRequireSignaturesFile {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let state_str = std::str::from_utf8(&data).map_err(|_| errno!(EINVAL))?;
        let clean_state_str = state_str.split('\n').next().unwrap_or("");
        if clean_state_str != "0" {
            return Err(errno!(EINVAL));
        }
        Ok(())
    }
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(Cow::Borrowed(&b"0\n"[..]))
    }
}

pub struct SocketLimits {
    /// The maximum backlog size for a socket.
    pub max_connections: AtomicI32,
}

pub struct SystemLimits {
    /// Limits applied to inotify objects.
    pub inotify: InotifyLimits,

    /// Limits applied to socket objects.
    pub socket: SocketLimits,

    /// The maximum size of pipes in the system.
    pub pipe_max_size: AtomicUsize,

    /// Whether IoUring is disabled.
    ///
    ///  0 -> io_uring is enabled (default)
    ///  1 -> io_uring is enabled for processes in the io_uring_group
    ///  2 -> io_uring is disabled
    ///
    /// See https://docs.kernel.org/admin-guide/sysctl/kernel.html#io-uring-disabled
    pub io_uring_disabled: AtomicI32,

    /// If io_uring_disabled is 1, then io_uring is enabled only for processes with CAP_SYS_ADMIN
    /// or that are members of this group.
    ///
    /// See https://docs.kernel.org/admin-guide/sysctl/kernel.html#io-uring-group
    pub io_uring_group: AtomicI32,
}

impl Default for SystemLimits {
    fn default() -> SystemLimits {
        SystemLimits {
            inotify: InotifyLimits {
                max_queued_events: AtomicI32::new(16384),
                max_user_instances: AtomicI32::new(128),
                max_user_watches: AtomicI32::new(1048576),
            },
            socket: SocketLimits { max_connections: AtomicI32::new(4096) },
            pipe_max_size: AtomicUsize::new((*PAGE_SIZE * 256) as usize),
            io_uring_disabled: AtomicI32::new(0),
            io_uring_group: AtomicI32::new(-1),
        }
    }
}

#[derive(Default)]
struct StubSysctl {
    data: Mutex<Vec<u8>>,
}

impl StubSysctl {
    #[track_caller]
    fn new_node(message: &'static str, bug: starnix_logging::BugRef) -> impl FsNodeOps {
        let location = std::panic::Location::caller();
        let file = BytesFile::new(Self::default());
        SimpleFileNode::new(move || {
            starnix_logging::__track_stub_inner(bug, message, None, location);
            Ok(file.clone())
        })
    }

    #[track_caller]
    fn new_node_with_capabilities(
        message: &'static str,
        bug: starnix_logging::BugRef,
        capabilities: Capabilities,
    ) -> impl FsNodeOps {
        let location = std::panic::Location::caller();
        let file = BytesFile::new(Self::default());
        SimpleFileNode::new_with_capabilities(
            move || {
                starnix_logging::__track_stub_inner(bug, message, None, location);
                Ok(file.clone())
            },
            capabilities,
        )
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
        StubSysctl::new_node("/proc/net/fib_trie", bug_ref!("https://fxbug.dev/322873635")),
        mode!(IFREG, 0o400),
    );
    dir.entry(
        current_task,
        "if_inet6",
        StubSysctl::new_node("/proc/net/if_inet6", bug_ref!("https://fxbug.dev/322874669")),
        mode!(IFREG, 0o444),
    );
    dir.entry(
        current_task,
        "ip_tables_names",
        BytesFile::new_node(b"nat\nfilter\nmangle\nraw\n".to_vec()),
        mode!(IFREG, 0o644),
    );
    dir.entry(
        current_task,
        "psched",
        StubSysctl::new_node("/proc/net/psched", bug_ref!("https://fxbug.dev/322874710")),
        mode!(IFREG, 0o444),
    );
    dir.entry(
        current_task,
        "xt_qtaguid",
        StubSysctl::new_node("/proc/net/xt_qtaguid", bug_ref!("https://fxbug.dev/322874322")),
        mode!(IFREG, 0o644),
    );
    dir.subdir(current_task, "xt_quota", 0o555, |dir| {
        dir.entry(
            current_task,
            "globalAlert",
            StubSysctl::new_node(
                "/proc/net/xt_quota/globalAlert",
                bug_ref!("https://fxbug.dev/322873636"),
            ),
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
                        bug_ref!("https://fxbug.dev/297439563"),
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
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "retrans_time_ms",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv4/DEVICE/neigh/retrans_time_ms",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "mcast_resolicit",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv4/DEVICE/neigh/mcast_resolicit",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "base_reachable_time_ms",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv4/DEVICE/neigh/base_reachable_time_ms",
                        bug_ref!("https://fxbug.dev/297439563"),
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
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "accept_ra_defrtr",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/accept_ra_defrtr",
                        bug_ref!("https://fxbug.dev/322907588"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "accept_ra_info_min_plen",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/accept_ra_info_min_plen",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "accept_ra_rt_info_min_plen",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/accept_ra_rt_info_min_plen",
                        bug_ref!("https://fxbug.dev/322908046"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "accept_ra_rt_table",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/accept_ra_rt_table",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "accept_redirects",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/accept_redirects",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "dad_transmits",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/dad_transmits",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "use_tempaddr",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/use_tempaddr",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "addr_gen_mode",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/addr_gen_mode",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "stable_secret",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/stable_secret",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "disable_ipv6",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/disable_ip64",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "optimistic_dad",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/optimistic_dad",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "use_oif_addrs_only",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/use_oif_addrs_only",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "use_optimistic",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/use_optimistic",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "forwarding",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/forwarding",
                        bug_ref!("https://fxbug.dev/322907925"),
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
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "retrans_time_ms",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/neigh/retrans_time_ms",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "mcast_resolicit",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/neigh/mcast_resolicit",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "base_reachable_time_ms",
                    StubSysctl::new_node(
                        "/proc/sys/net/ipv6/DEVICE/neigh/base_reachable_time_ms",
                        bug_ref!("https://fxbug.dev/297439563"),
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
            StubSysctl::new_node_with_capabilities(
                "/proc/sys/net/core/bpf_jit_enable",
                bug_ref!("https://fxbug.dev/322874627"),
                CAP_NET_ADMIN,
            ),
            file_mode,
        );
        dir.entry(
            current_task,
            "bpf_jit_kallsyms",
            StubSysctl::new_node_with_capabilities(
                "/proc/sys/net/core/bpf_jit_kallsyms",
                bug_ref!("https://fxbug.dev/322874163"),
                CAP_NET_ADMIN,
            ),
            file_mode,
        );
        dir.entry(
            current_task,
            "rmem_max",
            StubSysctl::new_node(
                "/proc/sys/net/core/rmem_max",
                bug_ref!("https://fxbug.dev/322906968"),
            ),
            file_mode,
        );
        dir.entry(current_task, "somaxconn", SystemLimitFile::<SoMaxConn>::new_node(), file_mode);
        dir.entry(
            current_task,
            "wmem_max",
            StubSysctl::new_node(
                "/proc/sys/net/core/wmem_max",
                bug_ref!("https://fxbug.dev/322907334"),
            ),
            file_mode,
        );
        dir.entry(
            current_task,
            "xfrm_acq_expires",
            StubSysctl::new_node(
                "/proc/sys/net/core/xfrm_acq_expires",
                bug_ref!("https://fxbug.dev/322907718"),
            ),
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
            StubSysctl::new_node(
                "/proc/sys/net/ipv4/fwmark_reflect",
                bug_ref!("https://fxbug.dev/322874495"),
            ),
            file_mode,
        );
        dir.entry(
            current_task,
            "ip_forward",
            StubSysctl::new_node(
                "/proc/sys/net/ipv4/ip_forward",
                bug_ref!("https://fxbug.dev/322874452"),
            ),
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
            StubSysctl::new_node(
                "/proc/sys/net/ipv4/ping_group_range",
                bug_ref!("https://fxbug.dev/322874256"),
            ),
            file_mode,
        );
        dir.entry(
            current_task,
            "tcp_default_init_rwnd",
            StubSysctl::new_node(
                "/proc/sys/net/ipv4/tcp_default_init_rwnd",
                bug_ref!("https://fxbug.dev/322874199"),
            ),
            file_mode,
        );
        dir.entry(
            current_task,
            "tcp_fwmark_accept",
            StubSysctl::new_node(
                "/proc/sys/net/ipv4/tcp_fwmark_accept",
                bug_ref!("https://fxbug.dev/322874120"),
            ),
            file_mode,
        );
        dir.entry(
            current_task,
            "tcp_rmem",
            StubSysctl::new_node(
                "/proc/sys/net/ipv4/tcp_rmem",
                bug_ref!("https://fxbug.dev/322874549"),
            ),
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
            StubSysctl::new_node(
                "/proc/sys/net/ipv6/fwmark_reflect",
                bug_ref!("https://fxbug.dev/322874711"),
            ),
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
            StubSysctl::new_node(
                "/proc/sys/net/unix/max_dgram_qlen",
                bug_ref!("https://fxbug.dev/322874454"),
            ),
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

trait AtomicLimit {
    type ValueType: std::str::FromStr + std::fmt::Display;

    fn load(current_task: &CurrentTask) -> Self::ValueType;
    fn store(current_task: &CurrentTask, value: Self::ValueType);
}

struct SystemLimitFile<T: AtomicLimit + Send + Sync + 'static> {
    marker: std::marker::PhantomData<T>,
}

impl<T: AtomicLimit + Send + Sync + 'static> SystemLimitFile<T>
where
    <T::ValueType as std::str::FromStr>::Err: std::fmt::Debug,
{
    pub fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self { marker: Default::default() })
    }
}

impl<T: AtomicLimit + Send + Sync + 'static> BytesFileOps for SystemLimitFile<T>
where
    <T::ValueType as std::str::FromStr>::Err: std::fmt::Debug,
{
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        // Is CAP_SYS_RESOURCE the correct capability for all these files?
        if !current_task.creds().has_capability(CAP_SYS_RESOURCE) {
            return error!(EPERM);
        }
        let value = fs_args::parse(FsString::from(data).as_ref())?;
        T::store(current_task, value);
        Ok(())
    }

    fn read(&self, current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(T::load(current_task).to_string().into_bytes().into())
    }
}

struct PipeMaxSize;
impl AtomicLimit for PipeMaxSize {
    type ValueType = usize;

    fn load(current_task: &CurrentTask) -> usize {
        current_task.kernel().system_limits.pipe_max_size.load(Ordering::Relaxed)
    }
    fn store(current_task: &CurrentTask, value: usize) {
        current_task.kernel().system_limits.pipe_max_size.store(value, Ordering::Relaxed);
    }
}

struct SoMaxConn;
impl AtomicLimit for SoMaxConn {
    type ValueType = i32;

    fn load(current_task: &CurrentTask) -> i32 {
        current_task.kernel().system_limits.socket.max_connections.load(Ordering::Relaxed)
    }
    fn store(current_task: &CurrentTask, value: i32) {
        current_task.kernel().system_limits.socket.max_connections.store(value, Ordering::Relaxed);
    }
}

struct IoUringDisabled;
impl AtomicLimit for IoUringDisabled {
    type ValueType = i32;

    fn load(current_task: &CurrentTask) -> i32 {
        current_task.kernel().system_limits.io_uring_disabled.load(Ordering::Relaxed)
    }
    fn store(current_task: &CurrentTask, value: i32) {
        if (0..=2).contains(&value) {
            current_task.kernel().system_limits.io_uring_disabled.store(value, Ordering::Relaxed);
        }
    }
}

struct IoUringGroup;
impl AtomicLimit for IoUringGroup {
    type ValueType = i32;

    fn load(current_task: &CurrentTask) -> i32 {
        current_task.kernel().system_limits.io_uring_group.load(Ordering::Relaxed)
    }
    fn store(current_task: &CurrentTask, value: i32) {
        current_task.kernel().system_limits.io_uring_group.store(value, Ordering::Relaxed);
    }
}
