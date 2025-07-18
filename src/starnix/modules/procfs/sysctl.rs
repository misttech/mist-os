// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::sys_net::{
    ProcSysNetIpv4Conf, ProcSysNetIpv4Neigh, ProcSysNetIpv6Conf, ProcSysNetIpv6Neigh,
};
use starnix_core::security;
use starnix_core::task::{ptrace_get_scope, ptrace_set_scope, CurrentTask, SeccompAction};
use starnix_core::vfs::pseudo::simple_directory::{SimpleDirectory, SimpleDirectoryMutator};
use starnix_core::vfs::pseudo::simple_file::{parse_unsigned_file, BytesFile, BytesFileOps};
use starnix_core::vfs::pseudo::stub_bytes_file::StubBytesFile;
use starnix_core::vfs::{fs_args, inotify, FileSystemHandle, FsNodeHandle, FsNodeOps, FsString};
use starnix_logging::bug_ref;
use starnix_uapi::auth::{CAP_LAST_CAP, CAP_NET_ADMIN, CAP_SYS_ADMIN, CAP_SYS_RESOURCE};
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::mode;
use starnix_uapi::version::{KERNEL_RELEASE, KERNEL_VERSION};
use starnix_uapi::{errno, error};
use std::borrow::Cow;
use std::sync::atomic::Ordering;
use uuid::Uuid;

pub fn sysctl_directory(fs: &FileSystemHandle) -> FsNodeHandle {
    let mode = mode!(IFREG, 0o644);
    let root_dir = SimpleDirectory::new();
    let dir = SimpleDirectoryMutator::new(fs.clone(), root_dir.clone());
    dir.subdir("abi", 0o555, |_dir| {
        #[cfg(target_arch = "aarch64")]
        _dir.entry(
            "swp",
            StubBytesFile::new_node("/proc/sys/abi/swp", bug_ref!("https://fxbug.dev/322873460")),
            mode,
        );
        #[cfg(target_arch = "aarch64")]
        _dir.entry(
            "tagged_addr_disabled",
            StubBytesFile::new_node(
                "/proc/sys/abi/tagged_addr_disabled",
                bug_ref!("https://fxbug.dev/408554469"),
            ),
            mode,
        );
    });
    dir.subdir("crypto", 0o555, |dir| {
        dir.entry("fips_enabled", BytesFile::new_node(b"0\n".to_vec()), mode!(IFREG, 0o444));
        dir.entry(
            "fips_name",
            BytesFile::new_node(b"Linux Kernel Cryptographic API\n".to_vec()),
            mode!(IFREG, 0o444),
        );
        dir.entry(
            "fips_version",
            BytesFile::new_node(|| Ok(format!("{}\n", KERNEL_VERSION))),
            mode!(IFREG, 0o444),
        );
    });
    dir.subdir("kernel", 0o555, |dir| {
        dir.entry(
            "cap_last_cap",
            BytesFile::new_node(|| Ok(format!("{}\n", CAP_LAST_CAP))),
            mode!(IFREG, 0o444),
        );
        dir.entry(
            "core_pattern",
            // TODO(https://fxbug.dev/322873960): Use the core pattern when generating a core dump.
            BytesFile::new_node(b"core".to_vec()),
            mode,
        );
        dir.entry(
            "core_pipe_limit",
            StubBytesFile::new_node(
                "/proc/sys/kernel/core_pipe_limit",
                bug_ref!("https://fxbug.dev/322873721"),
            ),
            mode,
        );
        dir.entry(
            "dmsg_restrict",
            StubBytesFile::new_node(
                "/proc/sys/kernel/dmsg_restrict",
                bug_ref!("https://fxbug.dev/322874424"),
            ),
            mode,
        );
        dir.entry(
            "domainname",
            StubBytesFile::new_node_with_data(
                "/proc/sys/kernel/domainname",
                bug_ref!("https://fxbug.dev/322873722"),
                "(none)",
            ),
            mode,
        );
        dir.entry(
            "hostname",
            StubBytesFile::new_node(
                "/proc/sys/kernel/hostname",
                bug_ref!("https://fxbug.dev/322873462"),
            ),
            mode,
        );
        dir.entry(
            "hung_task_check_count",
            StubBytesFile::new_node(
                "/proc/sys/kernel/hung_task_check_count",
                bug_ref!("https://fxbug.dev/322873962"),
            ),
            mode,
        );
        dir.entry(
            "hung_task_panic",
            StubBytesFile::new_node(
                "/proc/sys/kernel/hung_task_panic",
                bug_ref!("https://fxbug.dev/322873962"),
            ),
            mode,
        );
        dir.entry(
            "hung_task_timeout_secs",
            StubBytesFile::new_node(
                "/proc/sys/kernel/hung_task_timeout_secs",
                bug_ref!("https://fxbug.dev/322873962"),
            ),
            mode,
        );
        dir.entry(
            "hung_task_warnings",
            StubBytesFile::new_node(
                "/proc/sys/kernel/hung_task_warnings",
                bug_ref!("https://fxbug.dev/322873962"),
            ),
            mode,
        );
        dir.entry("io_uring_disabled", SystemLimitFile::<IoUringDisabled>::new_node(), mode);
        dir.entry("io_uring_group", SystemLimitFile::<IoUringGroup>::new_node(), mode);
        dir.entry(
            "modprobe",
            StubBytesFile::new_node(
                "/proc/sys/kernel/modprobe",
                bug_ref!("https://fxbug.dev/322874334"),
            ),
            mode,
        );
        dir.entry(
            "modules_disabled",
            StubBytesFile::new_node(
                "/proc/sys/kernel/modules_disabled",
                bug_ref!("https://fxbug.dev/322874489"),
            ),
            mode,
        );
        dir.entry("ngroups_max", BytesFile::new_node(b"65536\n".to_vec()), mode!(IFREG, 0o444));
        dir.entry(
            "panic_on_oops",
            StubBytesFile::new_node(
                "/proc/sys/kernel/panic_on_oops",
                bug_ref!("https://fxbug.dev/322874296"),
            ),
            mode,
        );
        dir.entry(
            "perf_cpu_time_max_percent",
            StubBytesFile::new_node(
                "/proc/sys/kernel/perf_cpu_time_max_percent",
                bug_ref!("https://fxbug.dev/322873262"),
            ),
            mode,
        );
        dir.entry(
            "perf_event_max_sample_rate",
            StubBytesFile::new_node(
                "/proc/sys/kernel/perf_event_max_sample_rate",
                bug_ref!("https://fxbug.dev/322874604"),
            ),
            mode,
        );
        dir.entry(
            "perf_event_mlock_kb",
            StubBytesFile::new_node(
                "/proc/sys/kernel/perf_event_mlock_kb",
                bug_ref!("https://fxbug.dev/322873800"),
            ),
            mode,
        );
        dir.entry(
            "perf_event_paranoid",
            StubBytesFile::new_node(
                "/proc/sys/kernel/perf_event_paranoid",
                bug_ref!("https://fxbug.dev/322873896"),
            ),
            mode,
        );
        dir.entry(
            "randomize_va_space",
            StubBytesFile::new_node(
                "/proc/sys/kernel/randomize_va_space",
                bug_ref!("https://fxbug.dev/322873202"),
            ),
            mode,
        );
        dir.entry(
            "sched_child_runs_first",
            StubBytesFile::new_node(
                "/proc/sys/kernel/sched_child_runs_first",
                bug_ref!("https://fxbug.dev/322874709"),
            ),
            mode,
        );
        dir.entry(
            "sched_latency_ns",
            StubBytesFile::new_node(
                "/proc/sys/kernel/sched_latency_ns",
                bug_ref!("https://fxbug.dev/322874319"),
            ),
            mode,
        );
        dir.entry(
            "sched_rt_period_us",
            StubBytesFile::new_node(
                "/proc/sys/kernel/sched_rt_period_us",
                bug_ref!("https://fxbug.dev/322874785"),
            ),
            mode,
        );
        dir.entry(
            "sched_rt_runtime_us",
            StubBytesFile::new_node(
                "/proc/sys/kernel/sched_rt_runtime_us",
                bug_ref!("https://fxbug.dev/322874726"),
            ),
            mode,
        );
        dir.entry(
            "sched_schedstats",
            StubBytesFile::new_node(
                "/proc/sys/kernel/sched_schedstats",
                bug_ref!("https://fxbug.dev/322874584"),
            ),
            mode,
        );
        dir.entry(
            "sched_tunable_scaling",
            StubBytesFile::new_node(
                "/proc/sys/kernel/sched_tunable_scaling",
                bug_ref!("https://fxbug.dev/322874666"),
            ),
            mode,
        );
        dir.entry(
            "sched_wakeup_granularity_ns",
            StubBytesFile::new_node(
                "/proc/sys/kernel/sched_wakeup_granularity_ns",
                bug_ref!("https://fxbug.dev/322874525"),
            ),
            mode,
        );
        dir.entry(
            "sysrq",
            StubBytesFile::new_node(
                "/proc/sys/kernel/sysrq",
                bug_ref!("https://fxbug.dev/322874375"),
            ),
            mode,
        );
        dir.entry("unprivileged_bpf_disabled", UnprivilegedBpfDisabled::new_node(), mode);
        dir.entry("dmesg_restrict", DmesgRestrict::new_node(), mode);
        dir.entry(
            "kptr_restrict",
            StubBytesFile::new_node(
                "/proc/sys/kernel/kptr_restrict",
                bug_ref!("https://fxbug.dev/322873878"),
            ),
            mode,
        );
        dir.entry(
            "osrelease",
            BytesFile::new_node(|| Ok(format!("{}\n", KERNEL_RELEASE))),
            mode!(IFREG, 0o444),
        );
        dir.entry("ostype", BytesFile::new_node(b"Linux\n".to_vec()), mode!(IFREG, 0o444));
        dir.entry("overflowuid", BytesFile::new_node(b"65534".to_vec()), mode);
        dir.entry("overflowgid", BytesFile::new_node(b"65534".to_vec()), mode);
        dir.entry("printk", BytesFile::new_node(b"4\t4\t1\t7\n".to_vec()), mode);
        dir.entry("pid_max", BytesFile::new_node(b"4194304".to_vec()), mode);
        dir.subdir("random", 0o555, |dir| {
            // Generate random UUID
            let boot_id = Uuid::new_v4().hyphenated().to_string();
            dir.entry("boot_id", BytesFile::new_node(boot_id.as_bytes().to_vec()), mode);
            dir.entry("entropy_avail", BytesFile::new_node(b"256".to_vec()), mode!(IFREG, 0o444));
        });
        dir.entry("tainted", KernelTaintedFile::new_node(), mode);
        dir.subdir("seccomp", 0o555, |dir| {
            dir.entry(
                "actions_avail",
                BytesFile::new_node(SeccompAction::get_actions_avail_file()),
                mode!(IFREG, 0o444),
            );
            dir.entry("actions_logged", SeccompActionsLogged::new_node(), mode);
        });
        dir.subdir("yama", 0o555, |dir| {
            dir.entry("ptrace_scope", PtraceYamaScope::new_node(), mode);
        });
    });
    dir.subdir("net", 0o555, sysctl_net_diretory);
    dir.entry(
        "version",
        BytesFile::new_node(|| Ok(format!("{}\n", KERNEL_VERSION))),
        mode!(IFREG, 0o444),
    );
    dir.subdir("vm", 0o555, |dir| {
        dir.entry(
            "dirty_background_ratio",
            StubBytesFile::new_node(
                "/proc/sys/vm/dirty_background_ratio",
                bug_ref!("https://fxbug.dev/322874492"),
            ),
            mode,
        );
        dir.entry(
            "dirty_expire_centisecs",
            StubBytesFile::new_node(
                "/proc/sys/vm/dirty_expire_centisecs",
                bug_ref!("https://fxbug.dev/322874237"),
            ),
            mode,
        );
        dir.entry(
            "drop_caches",
            StubBytesFile::new_node(
                "/proc/sys/vm/drop_caches",
                bug_ref!("https://fxbug.dev/322874299"),
            ),
            mode,
        );
        dir.entry(
            "extra_free_kbytes",
            StubBytesFile::new_node(
                "/proc/sys/vm/extra_free_kbytes",
                bug_ref!("https://fxbug.dev/322873761"),
            ),
            mode,
        );
        dir.entry(
            "max_map_count",
            StubBytesFile::new_node(
                "/proc/sys/vm/max_map_count",
                bug_ref!("https://fxbug.dev/322874684"),
            ),
            mode,
        );
        dir.entry(
            "mmap_min_addr",
            StubBytesFile::new_node(
                "/proc/sys/vm/mmap_min_addr",
                bug_ref!("https://fxbug.dev/322874526"),
            ),
            mode,
        );
        dir.entry(
            "mmap_rnd_bits",
            StubBytesFile::new_node(
                "/proc/sys/vm/mmap_rnd_bits",
                bug_ref!("https://fxbug.dev/322874505"),
            ),
            mode,
        );
        dir.entry(
            "mmap_rnd_compat_bits",
            StubBytesFile::new_node(
                "/proc/sys/vm/mmap_rnd_compat_bits",
                bug_ref!("https://fxbug.dev/322874685"),
            ),
            mode,
        );
        dir.entry(
            "overcommit_memory",
            StubBytesFile::new_node(
                "/proc/sys/vm/overcommit_memory",
                bug_ref!("https://fxbug.dev/322874159"),
            ),
            mode,
        );
        dir.entry(
            "page-cluster",
            StubBytesFile::new_node(
                "/proc/sys/vm/page-cluster",
                bug_ref!("https://fxbug.dev/322874302"),
            ),
            mode,
        );
        dir.entry(
            "watermark_scale_factor",
            StubBytesFile::new_node(
                "/proc/sys/vm/watermark_scale_factor",
                bug_ref!("https://fxbug.dev/322874321"),
            ),
            mode,
        );
    });
    dir.subdir("fs", 0o555, |dir| {
        dir.subdir("inotify", 0o555, |dir| {
            dir.entry("max_queued_events", inotify::InotifyMaxQueuedEvents::new_node(), mode);
            dir.entry("max_user_instances", inotify::InotifyMaxUserInstances::new_node(), mode);
            dir.entry("max_user_watches", inotify::InotifyMaxUserWatches::new_node(), mode);
        });
        dir.entry("pipe-max-size", SystemLimitFile::<PipeMaxSize>::new_node(), mode);
        dir.entry(
            "protected_hardlinks",
            StubBytesFile::new_node(
                "/proc/sys/fs/protected_hardlinks",
                bug_ref!("https://fxbug.dev/322874347"),
            ),
            mode,
        );
        dir.entry(
            "protected_symlinks",
            StubBytesFile::new_node(
                "/proc/sys/fs/protected_symlinks",
                bug_ref!("https://fxbug.dev/322874764"),
            ),
            mode,
        );
        dir.entry(
            "suid_dumpable",
            StubBytesFile::new_node(
                "/proc/sys/fs/suid_dumpable",
                bug_ref!("https://fxbug.dev/322874210"),
            ),
            mode,
        );
        dir.subdir("verity", 0o555, |dir| {
            dir.entry("require_signatures", VerityRequireSignaturesFile::new_node(), mode);
        });
    });
    // TODO: Validate the mode bits are correct.
    root_dir.into_node(fs, 0o777)
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
            return error!(EINVAL);
        }
        Ok(())
    }
    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(Cow::Borrowed(&b"0\n"[..]))
    }
}

pub fn net_directory(fs: &FileSystemHandle) -> FsNodeHandle {
    let dir = SimpleDirectory::new();
    dir.edit(fs, |dir| {
        dir.entry(
            "fib_trie",
            StubBytesFile::new_node("/proc/net/fib_trie", bug_ref!("https://fxbug.dev/322873635")),
            mode!(IFREG, 0o400),
        );
        dir.entry(
            "if_inet6",
            StubBytesFile::new_node("/proc/net/if_inet6", bug_ref!("https://fxbug.dev/322874669")),
            mode!(IFREG, 0o444),
        );
        dir.entry(
            "ip_tables_names",
            BytesFile::new_node(b"nat\nfilter\nmangle\nraw\n".to_vec()),
            mode!(IFREG, 0o644),
        );
        dir.entry(
            "ip6_tables_names",
            BytesFile::new_node(b"filter\nmangle\nraw\n".to_vec()),
            mode!(IFREG, 0o644),
        );
        dir.entry(
            "psched",
            StubBytesFile::new_node("/proc/net/psched", bug_ref!("https://fxbug.dev/322874710")),
            mode!(IFREG, 0o444),
        );
        dir.entry(
            "xt_qtaguid",
            StubBytesFile::new_node(
                "/proc/net/xt_qtaguid",
                bug_ref!("https://fxbug.dev/322874322"),
            ),
            mode!(IFREG, 0o644),
        );
        dir.subdir("xt_quota", 0o555, |dir| {
            dir.entry(
                "globalAlert",
                StubBytesFile::new_node(
                    "/proc/net/xt_quota/globalAlert",
                    bug_ref!("https://fxbug.dev/322873636"),
                ),
                mode!(IFREG, 0o444),
            );
        });
    });
    // TODO: Validate the mode bits are correct.
    dir.into_node(fs, 0o777)
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

fn sysctl_net_diretory(dir: &SimpleDirectoryMutator) {
    let file_mode = mode!(IFREG, 0o644);
    let dir_mode = mode!(IFDIR, 0o644);

    dir.subdir("core", 0o555, |dir| {
        dir.entry(
            "bpf_jit_enable",
            StubBytesFile::new_node_with_capabilities(
                "/proc/sys/net/core/bpf_jit_enable",
                bug_ref!("https://fxbug.dev/322874627"),
                CAP_NET_ADMIN,
            ),
            file_mode,
        );
        dir.entry(
            "bpf_jit_kallsyms",
            StubBytesFile::new_node_with_capabilities(
                "/proc/sys/net/core/bpf_jit_kallsyms",
                bug_ref!("https://fxbug.dev/322874163"),
                CAP_NET_ADMIN,
            ),
            file_mode,
        );
        dir.entry(
            "rmem_max",
            StubBytesFile::new_node(
                "/proc/sys/net/core/rmem_max",
                bug_ref!("https://fxbug.dev/322906968"),
            ),
            file_mode,
        );
        dir.entry("somaxconn", SystemLimitFile::<SoMaxConn>::new_node(), file_mode);
        dir.entry(
            "wmem_max",
            StubBytesFile::new_node(
                "/proc/sys/net/core/wmem_max",
                bug_ref!("https://fxbug.dev/322907334"),
            ),
            file_mode,
        );
        dir.entry(
            "xfrm_acq_expires",
            StubBytesFile::new_node(
                "/proc/sys/net/core/xfrm_acq_expires",
                bug_ref!("https://fxbug.dev/322907718"),
            ),
            file_mode,
        );
    });
    dir.subdir("ipv4", 0o555, |dir| {
        dir.entry("conf", ProcSysNetIpv4Conf, dir_mode);
        dir.entry(
            "fwmark_reflect",
            StubBytesFile::new_node(
                "/proc/sys/net/ipv4/fwmark_reflect",
                bug_ref!("https://fxbug.dev/322874495"),
            ),
            file_mode,
        );
        dir.entry(
            "ip_forward",
            StubBytesFile::new_node(
                "/proc/sys/net/ipv4/ip_forward",
                bug_ref!("https://fxbug.dev/322874452"),
            ),
            file_mode,
        );
        dir.entry("neigh", ProcSysNetIpv4Neigh, dir_mode);
        dir.entry(
            "ping_group_range",
            StubBytesFile::new_node(
                "/proc/sys/net/ipv4/ping_group_range",
                bug_ref!("https://fxbug.dev/322874256"),
            ),
            file_mode,
        );
        dir.entry(
            "tcp_default_init_rwnd",
            StubBytesFile::new_node(
                "/proc/sys/net/ipv4/tcp_default_init_rwnd",
                bug_ref!("https://fxbug.dev/322874199"),
            ),
            file_mode,
        );
        dir.entry(
            "tcp_fwmark_accept",
            StubBytesFile::new_node(
                "/proc/sys/net/ipv4/tcp_fwmark_accept",
                bug_ref!("https://fxbug.dev/322874120"),
            ),
            file_mode,
        );
        dir.entry(
            "tcp_rmem",
            StubBytesFile::new_node(
                "/proc/sys/net/ipv4/tcp_rmem",
                bug_ref!("https://fxbug.dev/322874549"),
            ),
            file_mode,
        );
    });
    dir.subdir("ipv6", 0o555, |dir| {
        dir.entry("conf", ProcSysNetIpv6Conf, dir_mode);
        dir.entry(
            "fwmark_reflect",
            StubBytesFile::new_node(
                "/proc/sys/net/ipv6/fwmark_reflect",
                bug_ref!("https://fxbug.dev/322874711"),
            ),
            file_mode,
        );
        dir.entry("neigh", ProcSysNetIpv6Neigh, dir_mode);
    });
    dir.subdir("unix", 0o555, |dir| {
        dir.entry(
            "max_dgram_qlen",
            StubBytesFile::new_node(
                "/proc/sys/net/unix/max_dgram_qlen",
                bug_ref!("https://fxbug.dev/322874454"),
            ),
            file_mode,
        );
    });
}

struct DmesgRestrict {}

impl DmesgRestrict {
    fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self {})
    }
}

impl BytesFileOps for DmesgRestrict {
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        security::check_task_capable(current_task, CAP_SYS_ADMIN)?;
        let restrict = parse_unsigned_file::<u32>(&data)? != 0;
        current_task.kernel().restrict_dmesg.store(restrict, Ordering::Relaxed);
        Ok(())
    }
    fn read(&self, current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(format!("{}\n", current_task.kernel().restrict_dmesg.load(Ordering::Relaxed) as u32)
            .into_bytes()
            .into())
    }
}

struct UnprivilegedBpfDisabled {}

impl UnprivilegedBpfDisabled {
    fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self {})
    }
}

impl BytesFileOps for UnprivilegedBpfDisabled {
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        security::check_task_capable(current_task, CAP_SYS_ADMIN)?;
        if current_task.kernel().disable_unprivileged_bpf.load(Ordering::Relaxed) == 2 {
            return error!(EACCES);
        }
        let setting = parse_unsigned_file::<u8>(&data)?;
        current_task.kernel().disable_unprivileged_bpf.store(setting, Ordering::Relaxed);
        Ok(())
    }
    fn read(&self, current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        Ok(format!("{}\n", current_task.kernel().disable_unprivileged_bpf.load(Ordering::Relaxed))
            .into_bytes()
            .into())
    }
}

struct SeccompActionsLogged {}

impl SeccompActionsLogged {
    fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self {})
    }
}

impl BytesFileOps for SeccompActionsLogged {
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        security::check_task_capable(current_task, CAP_SYS_ADMIN)?;
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
        security::check_task_capable(current_task, CAP_SYS_ADMIN)?;
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
        security::check_task_capable(current_task, CAP_SYS_RESOURCE)?;
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
