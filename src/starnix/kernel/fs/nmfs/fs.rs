// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This file contains an implementation for storing network information in a file system.
//!
//! Each file within `/sys/fs/nmfs` represents a network and its properties.

use crate::task::CurrentTask;
use crate::vfs::{
    fileops_impl_seekable, CacheMode, FileObject, FileOps, FileSystem, FileSystemHandle,
    FileSystemOps, FileSystemOptions, FsNode, FsNodeHandle, FsNodeInfo, FsNodeOps, FsStr,
    InputBuffer, MemoryDirectoryFile, SimpleFileNode,
};
use serde::{Deserialize, Serialize};
use starnix_sync::{FileOpsCore, Locked, Mutex, WriteOps};
use starnix_uapi::auth::FsCred;
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::file_mode::FileMode;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::vfs::default_statfs;
use starnix_uapi::{errno, error, statfs};
use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

type NetworksHandle = Arc<Mutex<Networks>>;

/// Keeps track of networks and their [`NetworkMessage`].
#[derive(Default)]
pub struct Networks {
    networks: HashMap<u32, Option<NetworkMessage>>,
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
        let id = match name.to_string().parse::<u32>() {
            Ok(value) => value,
            Err(_) => return error!(EINVAL),
        };

        // Insert a new network entry, but don't populate any fields.
        let _ = self.networks.lock().networks.insert(id, None);

        let ops: Box<dyn FsNodeOps> = if mode.is_reg() {
            Box::new(NetworkFile::new_node(id, self.networks.clone()))
        } else {
            return error!(EACCES);
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
        _name: &FsStr,
        _child: &FsNodeHandle,
    ) -> Result<(), Errno> {
        error!(EPERM)
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
    id: u32,
    networks: NetworksHandle,
}

impl NetworkFile {
    pub fn new_node(id: u32, networks: NetworksHandle) -> impl FsNodeOps {
        SimpleFileNode::new(move || Ok(NetworkFile { id, networks: networks.clone() }))
    }
}

impl FileOps for NetworkFile {
    fileops_impl_seekable!();

    fn write(
        &self,
        _locked: &mut Locked<'_, WriteOps>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        _offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        let bytes = data.read_all()?;
        let json: NetworkMessage = serde_json::from_slice(&bytes).map_err(|_| errno!(EINVAL))?;

        let new_netid = json.netid;

        // The network id must be the same as the id listed in the JSON.
        if new_netid != self.id {
            return error!(EINVAL);
        }

        // Override the network if one existed previously for this id.
        let _network = self.networks.lock().networks.insert(self.id, Some(json));

        Ok(bytes.len())
    }

    fn read(
        &self,
        _locked: &mut Locked<'_, FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn crate::vfs::OutputBuffer,
    ) -> Result<usize, Errno> {
        let properties_str = match &self.networks.lock().networks.get(&self.id) {
            Some(Some(properties)) => serde_json::to_string(properties).map_err(|_| errno!(EIO))?,
            // A file that was created but hasn't yet
            // been populated with network properties.
            Some(None) => "{}".to_string(),
            None => return error!(ENOENT),
        };
        let bytes = properties_str.as_bytes();

        if offset >= bytes.len() {
            return Ok(0);
        }
        data.write(&bytes[offset..])
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
