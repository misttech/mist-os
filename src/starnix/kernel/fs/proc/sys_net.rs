// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::task::CurrentTask;
use crate::vfs::stub_bytes_file::StubBytesFile;
use crate::vfs::{FileSystemHandle, FsNodeHandle, StaticDirectoryBuilder};
use starnix_logging::bug_ref;
use starnix_uapi::file_mode::mode;

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
                    StubBytesFile::new_node(
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
                    StubBytesFile::new_node(
                        "/proc/sys/net/ipv4/DEVICE/neigh/ucast_solicit",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "retrans_time_ms",
                    StubBytesFile::new_node(
                        "/proc/sys/net/ipv4/DEVICE/neigh/retrans_time_ms",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "mcast_resolicit",
                    StubBytesFile::new_node(
                        "/proc/sys/net/ipv4/DEVICE/neigh/mcast_resolicit",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "base_reachable_time_ms",
                    StubBytesFile::new_node(
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
                    StubBytesFile::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/accept_ra",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "accept_ra_defrtr",
                    StubBytesFile::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/accept_ra_defrtr",
                        bug_ref!("https://fxbug.dev/322907588"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "accept_ra_info_min_plen",
                    StubBytesFile::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/accept_ra_info_min_plen",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "accept_ra_rt_info_min_plen",
                    StubBytesFile::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/accept_ra_rt_info_min_plen",
                        bug_ref!("https://fxbug.dev/322908046"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "accept_ra_rt_table",
                    StubBytesFile::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/accept_ra_rt_table",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "accept_redirects",
                    StubBytesFile::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/accept_redirects",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "dad_transmits",
                    StubBytesFile::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/dad_transmits",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "use_tempaddr",
                    StubBytesFile::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/use_tempaddr",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "addr_gen_mode",
                    StubBytesFile::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/addr_gen_mode",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "stable_secret",
                    StubBytesFile::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/stable_secret",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "disable_ipv6",
                    StubBytesFile::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/disable_ip64",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "optimistic_dad",
                    StubBytesFile::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/optimistic_dad",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "use_oif_addrs_only",
                    StubBytesFile::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/use_oif_addrs_only",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "use_optimistic",
                    StubBytesFile::new_node(
                        "/proc/sys/net/ipv6/DEVICE/conf/use_optimistic",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "forwarding",
                    StubBytesFile::new_node(
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
                    StubBytesFile::new_node(
                        "/proc/sys/net/ipv6/DEVICE/neigh/ucast_solicit",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "retrans_time_ms",
                    StubBytesFile::new_node(
                        "/proc/sys/net/ipv6/DEVICE/neigh/retrans_time_ms",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "mcast_resolicit",
                    StubBytesFile::new_node(
                        "/proc/sys/net/ipv6/DEVICE/neigh/mcast_resolicit",
                        bug_ref!("https://fxbug.dev/297439563"),
                    ),
                    file_mode,
                );
                dir.entry(
                    current_task,
                    "base_reachable_time_ms",
                    StubBytesFile::new_node(
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
