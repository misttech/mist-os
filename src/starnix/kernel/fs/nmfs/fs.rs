// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This file contains an implementation for storing network information in a file system.
//!
//! Each file within `/sys/fs/nmfs` represents a network and its properties.

use crate::task::{CurrentTask, Kernel};
use crate::vfs::fs_args::parse;
use crate::vfs::{
    BytesFile, BytesFileOps, CacheMode, FileOps, FileSystem, FileSystemHandle, FileSystemOps,
    FileSystemOptions, FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr, MemoryDirectoryFile,
};
use serde::{Deserialize, Serialize};
use starnix_sync::{FileOpsCore, Locked, Unlocked};
use starnix_types::vfs::default_statfs;
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::FileMode;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{errno, error, statfs};
use std::borrow::Cow;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use {fidl_fuchsia_net as fnet, fidl_fuchsia_netpol_socketproxy as fnp_socketproxy};

const DEFAULT_NETWORK_FILE_NAME: &str = "default";

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub(crate) struct NetworkMessage {
    pub(crate) netid: u32,
    pub(crate) mark: u32,
    pub(crate) handle: u64,
    #[serde(with = "addr_list")]
    pub(crate) dnsv4: Vec<Ipv4Addr>,
    #[serde(with = "addr_list")]
    pub(crate) dnsv6: Vec<Ipv6Addr>,

    #[serde(flatten)]
    pub(crate) versioned_properties: VersionedProperties,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(tag = "version")]
pub(crate) enum VersionedProperties {
    #[default]
    V1,
}

pub struct Nmfs;
impl Nmfs {
    pub fn new_fs(kernel: &Arc<Kernel>, options: FileSystemOptions) -> FileSystemHandle {
        let fs = FileSystem::new(kernel, CacheMode::Permanent, Nmfs, options)
            .expect("nmfs constructed with valid options");

        let node = FsNode::new_root(NetworkDirectoryNode::new());
        fs.set_root_node(node);
        fs
    }
}

const NMFS_NAME: &[u8; 4] = b"nmfs";
const NMFS_MAGIC: u32 = u32::from_be_bytes(*NMFS_NAME);

impl FileSystemOps for Nmfs {
    fn statfs(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _fs: &FileSystem,
        _current_task: &CurrentTask,
    ) -> Result<statfs, Errno> {
        Ok(default_statfs(NMFS_MAGIC))
    }

    fn name(&self) -> &'static FsStr {
        NMFS_NAME.into()
    }
}

pub fn nmfs(
    _locked: &mut Locked<'_, Unlocked>,
    current_task: &CurrentTask,
    options: FileSystemOptions,
) -> Result<FileSystemHandle, Errno> {
    struct NmfsHandle(FileSystemHandle);

    let kernel = current_task.kernel();
    Ok(kernel.expando.get_or_init(|| NmfsHandle(Nmfs::new_fs(&kernel, options))).0.clone())
}

pub struct NetworkDirectoryNode;

impl NetworkDirectoryNode {
    pub fn new() -> Self {
        Self
    }
}

impl FsNodeOps for NetworkDirectoryNode {
    fn create_file_ops(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(MemoryDirectoryFile::new()))
    }

    fn mkdir(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _mode: FileMode,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        error!(EPERM)
    }

    fn mknod(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        mode: FileMode,
        _dev: DeviceType,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        if !mode.is_reg() {
            return error!(EACCES);
        }

        let ops: Box<dyn FsNodeOps> = if name == DEFAULT_NETWORK_FILE_NAME {
            // The node with DEFAULT_NETWORK_FILE_NAME is special and can
            // only be written to with network ids.
            Box::new(DefaultNetworkIdFile::new_node())
        } else {
            let id: u32 = parse(name).map_err(|_| errno!(EINVAL))?;
            // Insert a new network entry, but don't populate any fields.
            let network_manager = &current_task.kernel().network_manager.0;
            // This call should only occur on the first node with this name,
            // so this call isn't expected to fail.
            network_manager.add_empty_network(id)?;
            Box::new(NetworkFile::new_node(id))
        };

        let child = node.fs().create_node(
            current_task,
            ops,
            FsNodeInfo::new_factory(mode, current_task.as_fscred()),
        );

        Ok(child)
    }

    fn unlink(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        current_task: &CurrentTask,
        name: &FsStr,
        _child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        let network_manager = &current_task.kernel().network_manager.0;

        // Note: direct equality comparisons are easier using FsStr
        // than using a match block.
        if name == DEFAULT_NETWORK_FILE_NAME {
            // Reset the default network when the associated
            // network file with the same id is unlinked.
            network_manager.set_default_network_id(None);
        } else {
            let id: u32 = parse(name)?;
            network_manager.remove_network(id)?;
        }

        Ok(())
    }

    fn create_symlink(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _name: &FsStr,
        _target: &FsStr,
        _owner: FsCred,
    ) -> Result<FsNodeHandle, Errno> {
        error!(EPERM)
    }
}

pub struct NetworkFile {
    network_id: u32,
}

impl NetworkFile {
    pub fn new_node(network_id: u32) -> impl FsNodeOps {
        BytesFile::new_node(Self { network_id })
    }
}

impl BytesFileOps for NetworkFile {
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let network: NetworkMessage = serde_json::from_slice(&data).map_err(|_| errno!(EINVAL))?;

        let new_netid = network.netid;

        // The network id must be the same as the id listed in the JSON.
        if new_netid != self.network_id {
            return error!(EINVAL);
        }

        let network_manager = &current_task.kernel().network_manager.0;
        match network_manager.get_network(&new_netid) {
            None | Some(None) => {
                network_manager.add_network(network)?;
            }
            Some(Some(_old_network)) => {
                network_manager.update_network(network)?;
            }
        }

        Ok(())
    }

    fn read(&self, current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        let network_manager = &current_task.kernel().network_manager.0;
        // Verify whether the network exists before reading.
        if let None = network_manager.get_network(&self.network_id) {
            return error!(ENOENT);
        }

        Ok(network_manager.get_network_by_id_as_bytes(self.network_id).to_vec().into())
    }
}

pub struct DefaultNetworkIdFile {}

impl DefaultNetworkIdFile {
    pub fn new_node() -> impl FsNodeOps {
        BytesFile::new_node(Self {})
    }
}

impl BytesFileOps for DefaultNetworkIdFile {
    fn write(&self, current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let id_string = std::str::from_utf8(&data).map_err(|_| errno!(EINVAL))?;
        let id: u32 = id_string.parse().map_err(|_| errno!(EINVAL))?;

        {
            let network_manager = &current_task.kernel().network_manager.0;
            match network_manager.get_network(&id) {
                // A network with the provided id must already
                // exist to become the default network.
                Some(Some(_)) => {
                    network_manager.set_default_network_id(Some(id));
                }
                // The network properties must be provided for
                // a network before it can become the default.
                Some(None) | None => return error!(ENOENT),
            };
        }
        Ok(())
    }

    fn read(&self, current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        let network_manager = &current_task.kernel().network_manager.0;
        if let None = network_manager.get_default_network_id() {
            return error!(ENOENT);
        }

        Ok(network_manager.get_default_id_as_bytes().to_vec().into())
    }
}

impl From<&NetworkMessage> for fnp_socketproxy::Network {
    fn from(message: &NetworkMessage) -> Self {
        Self {
            network_id: Some(message.netid),
            info: Some(fnp_socketproxy::NetworkInfo::Starnix(
                fnp_socketproxy::StarnixNetworkInfo {
                    mark: Some(message.mark),
                    handle: Some(message.handle),
                    ..Default::default()
                },
            )),
            dns_servers: Some(fnp_socketproxy::NetworkDnsServers {
                v4: Some(
                    message
                        .dnsv4
                        .clone()
                        .into_iter()
                        .map(|a| fnet::Ipv4Address { addr: a.octets() })
                        .collect::<Vec<_>>(),
                ),
                v6: Some(
                    message
                        .dnsv6
                        .clone()
                        .into_iter()
                        .map(|a| fnet::Ipv6Address { addr: a.octets() })
                        .collect::<Vec<_>>(),
                ),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

mod addr_list {
    use serde::{de, Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S, Addr>(addr: &Vec<Addr>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
        Addr: std::fmt::Display,
    {
        let strings = addr.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        strings.serialize(serializer)
    }

    pub fn deserialize<'de, D, Addr>(deserializer: D) -> Result<Vec<Addr>, D::Error>
    where
        D: Deserializer<'de>,
        Addr: std::str::FromStr,
        Addr::Err: std::fmt::Display,
    {
        let s = Vec::<String>::deserialize(deserializer)?;
        s.into_iter().map(|s| s.parse().map_err(de::Error::custom)).collect()
    }
}
