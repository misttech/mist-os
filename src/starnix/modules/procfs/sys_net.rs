// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use netlink::SysctlInterfaceSelector;
use starnix_core::task::CurrentTask;
use starnix_core::vfs::pseudo::simple_file::{
    parse_i32_file, serialize_for_file, BytesFile, BytesFileOps, SimpleFileNode,
};
use starnix_core::vfs::pseudo::static_directory::StaticDirectoryBuilder;
use starnix_core::vfs::pseudo::stub_bytes_file::StubBytesFile;
use starnix_core::vfs::{
    emit_dotdot, fileops_impl_directory, fileops_impl_noop_sync, fileops_impl_unbounded_seek,
    fs_node_impl_dir_readonly, DirectoryEntryType, DirentSink, FileObject, FileOps, FsNode,
    FsNodeHandle, FsNodeOps, FsStr,
};
use starnix_logging::{bug_ref, log_error};
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::{mode, FileMode};
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{errno, error};

const FILE_MODE: FileMode = mode!(IFREG, 0o644);

fn netstack_devices_readdir(
    _locked: &mut Locked<FileOpsCore>,
    file: &FileObject,
    current_task: &CurrentTask,
    sink: &mut dyn DirentSink,
) -> Result<(), Errno> {
    emit_dotdot(file, sink)?;

    if sink.offset() == 2 {
        sink.add(
            file.fs.allocate_ino(),
            sink.offset() + 1,
            DirectoryEntryType::from_mode(FILE_MODE),
            "all".into(),
        )?;
    }

    if sink.offset() == 3 {
        sink.add(
            file.fs.allocate_ino(),
            sink.offset() + 1,
            DirectoryEntryType::from_mode(FILE_MODE),
            "default".into(),
        )?;
    }

    let devices = current_task.kernel().netstack_devices.snapshot_devices();
    for (name, _) in devices.iter().skip(sink.offset() as usize - 4) {
        let inode_num = file.fs.allocate_ino();
        sink.add(
            inode_num,
            sink.offset() + 1,
            DirectoryEntryType::from_mode(FILE_MODE),
            name.as_ref(),
        )?;
    }
    Ok(())
}

fn get_netstack_device(
    current_task: &CurrentTask,
    name: &FsStr,
) -> Option<SysctlInterfaceSelector> {
    // Per https://www.kernel.org/doc/Documentation/networking/ip-sysctl.txt,
    //
    //   conf/default/*:
    //	   Change the interface-specific default settings.
    //
    //   conf/all/*:
    //	   Change all the interface-specific settings.
    //
    // Note that the all/default directories don't exist in `/sys/class/net`.
    if name == "all" {
        return Some(SysctlInterfaceSelector::All);
    }
    if name == "default" {
        return Some(SysctlInterfaceSelector::Default);
    }
    if let Some(dev) = current_task.kernel().netstack_devices.get_device(name) {
        return Some(SysctlInterfaceSelector::Id(dev.interface_id));
    }
    None
}

#[derive(Clone)]
pub struct ProcSysNetIpv4Conf;

impl FsNodeOps for ProcSysNetIpv4Conf {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(self.clone()))
    }

    fn lookup(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        if get_netstack_device(current_task, name).is_some() {
            let fs = node.fs();
            let mut dir = StaticDirectoryBuilder::new(&fs);
            dir.entry(
                "accept_redirects",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv4/DEVICE/conf/accept_redirects",
                    bug_ref!("https://fxbug.dev/423646442"),
                ),
                FILE_MODE,
            );
            return Ok(dir.build());
        }
        error!(ENOENT, "looking for {name}")
    }
}

impl FileOps for ProcSysNetIpv4Conf {
    fileops_impl_directory!();
    fileops_impl_noop_sync!();
    fileops_impl_unbounded_seek!();

    fn readdir(
        &self,
        locked: &mut Locked<FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        netstack_devices_readdir(locked, file, current_task, sink)
    }
}

#[derive(Clone)]
pub struct ProcSysNetIpv4Neigh;

impl FsNodeOps for ProcSysNetIpv4Neigh {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(self.clone()))
    }

    fn lookup(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        if get_netstack_device(current_task, name).is_some() {
            let fs = node.fs();
            let mut dir = StaticDirectoryBuilder::new(&fs);
            dir.entry(
                "ucast_solicit",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv4/DEVICE/neigh/ucast_solicit",
                    bug_ref!("https://fxbug.dev/423646444"),
                ),
                FILE_MODE,
            );
            dir.entry(
                "retrans_time_ms",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv4/DEVICE/neigh/retrans_time_ms",
                    bug_ref!("https://fxbug.dev/423645762"),
                ),
                FILE_MODE,
            );
            dir.entry(
                "mcast_resolicit",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv4/DEVICE/neigh/mcast_resolicit",
                    bug_ref!("https://fxbug.dev/423645992"),
                ),
                FILE_MODE,
            );
            dir.entry(
                "base_reachable_time_ms",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv4/DEVICE/neigh/base_reachable_time_ms",
                    bug_ref!("https://fxbug.dev/423645348"),
                ),
                FILE_MODE,
            );
            return Ok(dir.build());
        }
        error!(ENOENT, "looking for {name}")
    }
}

impl FileOps for ProcSysNetIpv4Neigh {
    fileops_impl_directory!();
    fileops_impl_noop_sync!();
    fileops_impl_unbounded_seek!();

    fn readdir(
        &self,
        locked: &mut Locked<FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        netstack_devices_readdir(locked, file, current_task, sink)
    }
}

#[derive(Clone)]
pub struct ProcSysNetIpv6Conf;

impl FsNodeOps for ProcSysNetIpv6Conf {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(self.clone()))
    }

    fn lookup(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        if let Some(interface) = get_netstack_device(current_task, name) {
            let fs = node.fs();
            let mut dir = StaticDirectoryBuilder::new(&fs);
            dir.entry(
                "accept_ra",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv6/DEVICE/conf/accept_ra",
                    bug_ref!("https://fxbug.dev/423646365"),
                ),
                FILE_MODE,
            );
            dir.entry(
                "accept_ra_defrtr",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv6/DEVICE/conf/accept_ra_defrtr",
                    bug_ref!("https://fxbug.dev/322907588"),
                ),
                FILE_MODE,
            );
            dir.entry(
                "accept_ra_info_min_plen",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv6/DEVICE/conf/accept_ra_info_min_plen",
                    bug_ref!("https://fxbug.dev/423645816"),
                ),
                FILE_MODE,
            );
            dir.entry(
                "accept_ra_rt_info_min_plen",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv6/DEVICE/conf/accept_ra_rt_info_min_plen",
                    bug_ref!("https://fxbug.dev/322908046"),
                ),
                FILE_MODE,
            );
            dir.entry(
                "accept_ra_rt_table",
                NetworkNetlinkSysctlFile::new_node(interface),
                FILE_MODE,
            );
            dir.entry(
                "accept_redirects",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv6/DEVICE/conf/accept_redirects",
                    bug_ref!("https://fxbug.dev/423646442"),
                ),
                FILE_MODE,
            );
            dir.entry(
                "dad_transmits",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv6/DEVICE/conf/dad_transmits",
                    bug_ref!("https://fxbug.dev/423646145"),
                ),
                FILE_MODE,
            );
            dir.entry(
                "use_tempaddr",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv6/DEVICE/conf/use_tempaddr",
                    bug_ref!("https://fxbug.dev/423646346"),
                ),
                FILE_MODE,
            );
            dir.entry(
                "addr_gen_mode",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv6/DEVICE/conf/addr_gen_mode",
                    bug_ref!("https://fxbug.dev/423645864"),
                ),
                FILE_MODE,
            );
            dir.entry(
                "stable_secret",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv6/DEVICE/conf/stable_secret",
                    bug_ref!("https://fxbug.dev/423646722"),
                ),
                FILE_MODE,
            );
            dir.entry(
                "disable_ipv6",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv6/DEVICE/conf/disable_ipv6",
                    bug_ref!("https://fxbug.dev/423645469"),
                ),
                FILE_MODE,
            );
            dir.entry(
                "optimistic_dad",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv6/DEVICE/conf/optimistic_dad",
                    bug_ref!("https://fxbug.dev/423646584"),
                ),
                FILE_MODE,
            );
            dir.entry(
                "use_oif_addrs_only",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv6/DEVICE/conf/use_oif_addrs_only",
                    bug_ref!("https://fxbug.dev/423645421"),
                ),
                FILE_MODE,
            );
            dir.entry(
                "use_optimistic",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv6/DEVICE/conf/use_optimistic",
                    bug_ref!("https://fxbug.dev/423645883"),
                ),
                FILE_MODE,
            );
            dir.entry(
                "forwarding",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv6/DEVICE/conf/forwarding",
                    bug_ref!("https://fxbug.dev/322907925"),
                ),
                FILE_MODE,
            );
            return Ok(dir.build());
        }
        error!(ENOENT, "looking for {name}")
    }
}

impl FileOps for ProcSysNetIpv6Conf {
    fileops_impl_directory!();
    fileops_impl_noop_sync!();
    fileops_impl_unbounded_seek!();

    fn readdir(
        &self,
        locked: &mut Locked<FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        netstack_devices_readdir(locked, file, current_task, sink)
    }
}

#[derive(Clone)]
pub struct ProcSysNetIpv6Neigh;

impl FsNodeOps for ProcSysNetIpv6Neigh {
    fs_node_impl_dir_readonly!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(self.clone()))
    }

    fn lookup(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
    ) -> Result<FsNodeHandle, Errno> {
        if get_netstack_device(current_task, name).is_some() {
            let fs = node.fs();
            let mut dir = StaticDirectoryBuilder::new(&fs);
            dir.entry(
                "ucast_solicit",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv6/DEVICE/neigh/ucast_solicit",
                    bug_ref!("https://fxbug.dev/423646444"),
                ),
                FILE_MODE,
            );
            dir.entry(
                "retrans_time_ms",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv6/DEVICE/neigh/retrans_time_ms",
                    bug_ref!("https://fxbug.dev/423645762"),
                ),
                FILE_MODE,
            );
            dir.entry(
                "mcast_resolicit",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv6/DEVICE/neigh/mcast_resolicit",
                    bug_ref!("https://fxbug.dev/423645992"),
                ),
                FILE_MODE,
            );
            dir.entry(
                "base_reachable_time_ms",
                StubBytesFile::new_node(
                    "/proc/sys/net/ipv6/DEVICE/neigh/base_reachable_time_ms",
                    bug_ref!("https://fxbug.dev/423645348"),
                ),
                FILE_MODE,
            );
            return Ok(dir.build());
        }
        error!(ENOENT, "looking for {name}")
    }
}

impl FileOps for ProcSysNetIpv6Neigh {
    fileops_impl_directory!();
    fileops_impl_noop_sync!();
    fileops_impl_unbounded_seek!();

    fn readdir(
        &self,
        locked: &mut Locked<FileOpsCore>,
        file: &FileObject,
        current_task: &CurrentTask,
        sink: &mut dyn DirentSink,
    ) -> Result<(), Errno> {
        netstack_devices_readdir(locked, file, current_task, sink)
    }
}

struct NetworkNetlinkSysctlFile {
    interface: SysctlInterfaceSelector,
}

impl NetworkNetlinkSysctlFile {
    fn new_node(interface: SysctlInterfaceSelector) -> impl FsNodeOps {
        SimpleFileNode::new(move || Ok(BytesFile::new(Self { interface })))
    }
}

impl BytesFileOps for NetworkNetlinkSysctlFile {
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let value = parse_i32_file(&data)?;
        current_task
            .kernel()
            .network_netlink()
            .write_accept_ra_rt_table(self.interface, value)
            .map_err(|err| {
                log_error!("failed to write to {:?}: {:?}", self.interface, err);
                errno!(EBADF)
            })
    }

    fn read(&self, current_task: &CurrentTask) -> Result<std::borrow::Cow<'_, [u8]>, Errno> {
        let value = current_task
            .kernel()
            .network_netlink()
            .read_accept_ra_rt_table(self.interface)
            .map_err(|err| {
                log_error!("failed to read from {:?}: {:?}", self.interface, err);
                errno!(EBADF)
            })?;
        Ok(serialize_for_file(value).into())
    }
}
