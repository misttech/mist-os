// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{auth::FsCred, fs::*, lock::Mutex, task::*, types::*};
use std::{borrow::Cow, sync::Arc};

pub fn sysctl_directory(fs: &FileSystemHandle, kernel: &Arc<Kernel>) -> FsNodeHandle {
    let mode = mode!(IFREG, 0o644);
    let mut dir = StaticDirectoryBuilder::new(fs);
    dir.subdir(b"kernel", 0o555, |dir| {
        dir.entry(b"unprivileged_bpf_disable", StubSysctl::new_node(), mode);
        dir.entry(b"kptr_restrict", StubSysctl::new_node(), mode);
        dir.node(
            b"overflowuid",
            fs.create_node(
                BytesFile::new_node(b"65534".to_vec()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o644), FsCred::root()),
            ),
        );
        dir.node(
            b"overflowgid",
            fs.create_node(
                BytesFile::new_node(b"65534".to_vec()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o644), FsCred::root()),
            ),
        );
        dir.node(
            b"pid_max",
            fs.create_node(
                BytesFile::new_node(b"4194304".to_vec()),
                FsNodeInfo::new_factory(mode!(IFREG, 0o644), FsCred::root()),
            ),
        );
        dir.entry(b"tainted", KernelTaintedFile::new_node(), mode);
    });
    dir.node(b"net", sysctl_net_diretory(fs, kernel));
    dir.subdir(b"vm", 0o555, |dir| {
        dir.entry(b"mmap_rnd_bits", StubSysctl::new_node(), mode);
        dir.entry(b"mmap_rnd_compat_bits", StubSysctl::new_node(), mode);
    });
    dir.build()
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

pub fn net_directory(fs: &FileSystemHandle) -> FsNodeHandle {
    let mut dir = StaticDirectoryBuilder::new(fs);
    dir.subdir(b"xt_quota", 0o555, |dir| {
        dir.entry(b"globalAlert", StubSysctl::new_node(), mode!(IFREG, 0o444));
    });
    dir.build()
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
        Ok(b"0\n".to_vec().into())
    }
}

/// Holds the device-specific directories that are found under `/proc/sys/net`.
pub struct ProcSysNetDev {
    /// The `/proc/sys/net/ipv4/{device}/neigh` directory.
    ipv4_neigh: Arc<FsNode>,
    /// The `/proc/sys/net/ipv6/{device}/conf` directory.
    ipv6_conf: Arc<FsNode>,
    /// The `/proc/sys/net/ipv6/{device}/neigh` directory.
    ipv6_neigh: Arc<FsNode>,
}

impl ProcSysNetDev {
    pub fn new(proc_fs: &FileSystemHandle) -> Self {
        let file_mode = mode!(IFREG, 0o644);
        // TODO(https://fxbug.dev/128995): Implement the "files" properly instead
        // of using stubs.
        ProcSysNetDev {
            ipv4_neigh: {
                let mut dir = StaticDirectoryBuilder::new(proc_fs);
                dir.entry(b"ucast_solicit", StubSysctl::new_node(), file_mode);
                dir.entry(b"retrans_time_ms", StubSysctl::new_node(), file_mode);
                dir.entry(b"mcast_resolicit", StubSysctl::new_node(), file_mode);
                dir.build()
            },
            ipv6_conf: {
                let mut dir = StaticDirectoryBuilder::new(proc_fs);
                dir.entry(b"accept_ra", StubSysctl::new_node(), file_mode);
                dir.entry(b"dad_transmits", StubSysctl::new_node(), file_mode);
                dir.entry(b"use_tempaddr", StubSysctl::new_node(), file_mode);
                dir.entry(b"addr_gen_mode", StubSysctl::new_node(), file_mode);
                dir.entry(b"stable_secret", StubSysctl::new_node(), file_mode);
                dir.entry(b"disable_ipv6", StubSysctl::new_node(), file_mode);
                dir.build()
            },
            ipv6_neigh: {
                let mut dir = StaticDirectoryBuilder::new(proc_fs);
                dir.entry(b"ucast_solicit", StubSysctl::new_node(), file_mode);
                dir.entry(b"retrans_time_ms", StubSysctl::new_node(), file_mode);
                dir.entry(b"mcast_resolicit", StubSysctl::new_node(), file_mode);
                dir.build()
            },
        }
    }

    pub fn get_ipv4_neigh(&self) -> &Arc<FsNode> {
        &self.ipv4_neigh
    }

    pub fn get_ipv6_conf(&self) -> &Arc<FsNode> {
        &self.ipv6_conf
    }

    pub fn get_ipv6_neigh(&self) -> &Arc<FsNode> {
        &self.ipv6_neigh
    }
}

fn sysctl_net_diretory(fs: &FileSystemHandle, kernel: &Arc<Kernel>) -> FsNodeHandle {
    let mut dir = StaticDirectoryBuilder::new(fs);
    let file_mode = mode!(IFREG, 0o644);
    let dir_mode = mode!(IFDIR, 0o644);

    let devs = &kernel.netstack_devices;
    // Per https://www.kernel.org/doc/Documentation/networking/ip-sysctl.txt,
    //
    //   conf/default/*:
    //	   Change the interface-specific default settings.
    //
    //   conf/all/*:
    //	   Change all the interface-specific settings.
    devs.add_dev("all", Some(fs));
    devs.add_dev("default", Some(fs));

    dir.subdir(b"core", 0o555, |dir| {
        dir.entry(b"bpf_jit_enable", StubSysctl::new_node(), file_mode);
        dir.entry(b"bpf_jit_kallsyms", StubSysctl::new_node(), file_mode);
    });
    dir.subdir(b"ipv4", 0o555, |dir| {
        dir.entry(
            b"neigh",
            NetstackDevicesDirectory::new_proc_sys_net_ipv4_neigh(devs.clone()),
            dir_mode,
        );
    });
    dir.subdir(b"ipv6", 0o555, |dir| {
        dir.entry(
            b"conf",
            NetstackDevicesDirectory::new_proc_sys_net_ipv6_conf(devs.clone()),
            dir_mode,
        );
        dir.entry(
            b"neigh",
            NetstackDevicesDirectory::new_proc_sys_net_ipv6_neigh(devs.clone()),
            dir_mode,
        );
    });
    dir.build()
}
