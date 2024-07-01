// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This file contains an implementation for storing network information in a file system.
//!
//! Each file within `/sys/fs/nmfs` represents a network and its properties.

use crate::task::CurrentTask;
use crate::vfs::fs_args::parse;
use crate::vfs::{
    BytesFile, BytesFileOps, CacheMode, FileOps, FileSystem, FileSystemHandle, FileSystemOps,
    FileSystemOptions, FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr, MemoryDirectoryFile,
};
use bstr::BString;
use serde::{Deserialize, Serialize};
use starnix_sync::{FileOpsCore, Locked, Mutex};
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::FileMode;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::vfs::default_statfs;
use starnix_uapi::{errno, error, statfs};
use std::borrow::Cow;
use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

type NetworksHandle = Arc<Mutex<Networks>>;

const DEFAULT_NETWORK_FILE_NAME: &str = "default";

/// Keeps track of networks and their [`NetworkMessage`].
#[derive(Default)]
pub struct Networks {
    default_id: Option<u32>,
    networks: HashMap<u32, Option<NetworkMessage>>,
}

impl Networks {
    pub fn get_default_id_as_bytes(&self) -> BString {
        let default_id = match self.default_id {
            Some(id) => id.to_string(),
            None => "".to_string(),
        };
        default_id.into_bytes().into()
    }

    pub fn get_network_by_id_as_bytes(&self, id: u32) -> BString {
        let network_info = match self.networks.get(&id) {
            Some(network) => serde_json::to_string(network).unwrap_or("{}".to_string()),
            // A network with that was created but hasn't yet
            // been populated with network properties.
            None => "{}".to_string(),
        };
        network_info.into_bytes().into()
    }
}

#[derive(Deserialize, Serialize)]
struct NetworkMessage {
    netid: u32,
    mark: u32,
    handle: u64,
    #[serde(with = "addr_list")]
    dnsv4: Vec<Ipv4Addr>,
    #[serde(with = "addr_list")]
    dnsv6: Vec<Ipv6Addr>,

    #[serde(flatten)]
    versioned_properties: VersionedProperties,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "version")]
enum VersionedProperties {
    V1,
}

pub struct Nmfs;
impl Nmfs {
    pub fn new_fs(current_task: &CurrentTask, options: FileSystemOptions) -> FileSystemHandle {
        let kernel = current_task.kernel();
        let fs = FileSystem::new(kernel, CacheMode::Permanent, Nmfs, options)
            .expect("nmfs constructed with valid options");

        let networks = Arc::new(Mutex::new(Networks::default()));

        let node = FsNode::new_root(NetworkDirectoryNode::new(networks.clone()));
        fs.set_root_node(node);
        fs
    }
}

const NMFS_NAME: &[u8; 4] = b"nmfs";
const NMFS_MAGIC: u32 = u32::from_be_bytes(*NMFS_NAME);

impl FileSystemOps for Nmfs {
    fn statfs(&self, _fs: &FileSystem, _current_task: &CurrentTask) -> Result<statfs, Errno> {
        Ok(default_statfs(NMFS_MAGIC))
    }

    fn name(&self) -> &'static FsStr {
        NMFS_NAME.into()
    }
}

pub fn nmfs(current_task: &CurrentTask, options: FileSystemOptions) -> &FileSystemHandle {
    current_task.kernel().nmfs.get_or_init(|| Nmfs::new_fs(current_task, options))
}

pub struct NetworkDirectoryNode {
    networks: NetworksHandle,
}

impl NetworkDirectoryNode {
    pub fn new(networks: Arc<Mutex<Networks>>) -> Self {
        Self { networks }
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
            Box::new(DefaultNetworkIdFile::new_node(self.networks.clone()))
        } else {
            let id: u32 = parse(name).map_err(|_| errno!(EINVAL))?;
            // Insert a new network entry, but don't populate any fields.
            let _ = self.networks.lock().networks.insert(id, None);
            Box::new(NetworkFile::new_node(id, self.networks.clone()))
        };

        let child =
            node.fs().create_node(current_task, ops, FsNodeInfo::new_factory(mode, FsCred::root()));

        Ok(child)
    }

    fn unlink(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        name: &FsStr,
        _child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        // Note: direct equality comparisons are easier using FsStr
        // than using a match block.
        if name == DEFAULT_NETWORK_FILE_NAME {
            // Reset the default network when the associated
            // network file with the same id is unlinked.
            self.networks.lock().default_id = None;
        } else {
            let mut binding = self.networks.lock();
            let id: u32 = parse(name)?;
            // The unlinked network must not be the current default network.
            if binding.default_id == Some(id) {
                return error!(EPERM);
            }

            // Remove the network from the HashMap when the associated file is unlinked.
            if let None = binding.networks.remove(&id) {
                return error!(ENOENT);
            }
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
    networks: NetworksHandle,
}

impl NetworkFile {
    pub fn new_node(network_id: u32, networks: NetworksHandle) -> impl FsNodeOps {
        BytesFile::new_node(Self { network_id, networks: networks.clone() })
    }
}

impl BytesFileOps for NetworkFile {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let json: NetworkMessage = serde_json::from_slice(&data).map_err(|_| errno!(EINVAL))?;

        let new_netid = json.netid;

        // The network id must be the same as the id listed in the JSON.
        if new_netid != self.network_id {
            return error!(EINVAL);
        }

        // Override the network if one existed previously for this id.
        let _network = self.networks.lock().networks.insert(self.network_id, Some(json));

        Ok(())
    }

    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        {
            // Verify whether the network exists before reading.
            let binding = self.networks.lock();
            if let None = binding.networks.get(&self.network_id) {
                return error!(ENOENT);
            }
        }

        Ok(self.networks.lock().get_network_by_id_as_bytes(self.network_id).to_vec().into())
    }
}

pub struct DefaultNetworkIdFile {
    networks: NetworksHandle,
}

impl DefaultNetworkIdFile {
    pub fn new_node(networks: NetworksHandle) -> impl FsNodeOps {
        BytesFile::new_node(Self { networks: networks.clone() })
    }
}

impl BytesFileOps for DefaultNetworkIdFile {
    fn write(&self, _current_task: &CurrentTask, data: Vec<u8>) -> Result<(), Errno> {
        let id_string = std::str::from_utf8(&data).map_err(|_| errno!(EINVAL))?;
        let id: u32 = id_string.parse().map_err(|_| errno!(EINVAL))?;

        {
            let mut binding = self.networks.lock();
            match binding.networks.get(&id) {
                // A network with the provided id must already
                // exist to become the default network.
                Some(Some(_)) => binding.default_id = Some(id),
                // The network properties must be provided for
                // a network before it can become the default.
                Some(None) | None => return error!(ENOENT),
            };
        }
        Ok(())
    }

    fn read(&self, _current_task: &CurrentTask) -> Result<Cow<'_, [u8]>, Errno> {
        if self.networks.lock().default_id.is_none() {
            return error!(ENOENT);
        }

        Ok(self.networks.lock().get_default_id_as_bytes().to_vec().into())
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
