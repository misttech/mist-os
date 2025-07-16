// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::DeviceMode;
use crate::task::CurrentTask;
use crate::vfs::buffers::{InputBuffer, OutputBuffer};
use crate::vfs::pseudo::simple_directory::SimpleDirectory;
use crate::vfs::{
    fileops_impl_noop_sync, fileops_impl_seekable, fs_node_impl_not_dir, FileObject, FileOps,
    FsNode, FsNodeOps, FsString, PathBuilder,
};
use starnix_logging::track_stub;
use starnix_sync::{FileOpsCore, Locked};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{errno, error};
use std::sync::Arc;

/// A Class is a higher-level view of a device.
///
/// It groups devices based on what they do, rather than how they are connected.
#[derive(Clone)]
pub struct Class {
    pub name: FsString,
    pub dir: Arc<SimpleDirectory>,
    /// Physical bus that the devices belong to.
    pub bus: Bus,
    pub collection: Arc<SimpleDirectory>,
}

impl Class {
    pub fn new(
        name: FsString,
        dir: Arc<SimpleDirectory>,
        bus: Bus,
        collection: Arc<SimpleDirectory>,
    ) -> Self {
        Self { name, dir, bus, collection }
    }
}

impl std::fmt::Debug for Class {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Class").field("name", &self.name).field("bus", &self.bus).finish()
    }
}

/// A Bus identifies how the devices are connected to the processor.
#[derive(Clone)]
pub struct Bus {
    pub name: FsString,
    pub dir: Arc<SimpleDirectory>,
    pub collection: Option<Arc<SimpleDirectory>>,
}

impl Bus {
    pub fn new(
        name: FsString,
        dir: Arc<SimpleDirectory>,
        collection: Option<Arc<SimpleDirectory>>,
    ) -> Self {
        Self { name, dir, collection }
    }
}

impl std::fmt::Debug for Bus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bus").field("name", &self.name).finish()
    }
}

#[derive(Clone, Debug)]
pub struct Device {
    pub name: FsString,
    pub class: Class,
    pub metadata: Option<DeviceMetadata>,
}

impl Device {
    pub fn new(name: FsString, class: Class, metadata: Option<DeviceMetadata>) -> Self {
        Self { name, class, metadata }
    }

    /// Returns a path to the device, relative to the sysfs root, going up `depth` directories.
    pub fn path_from_depth(&self, depth: usize) -> FsString {
        let mut builder = PathBuilder::new();
        builder.prepend_element(self.name.as_ref());
        builder.prepend_element(self.class.name.as_ref());
        builder.prepend_element(self.class.bus.name.as_ref());
        builder.prepend_element(b"devices".into());
        for _ in 0..depth {
            builder.prepend_element(b"..".into());
        }
        builder.build_relative()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceMetadata {
    /// Name of the device in /dev.
    ///
    /// Also appears in sysfs via uevent.
    pub devname: FsString,
    pub device_type: DeviceType,
    pub mode: DeviceMode,
}

impl DeviceMetadata {
    pub fn new(devname: FsString, device_type: DeviceType, mode: DeviceMode) -> Self {
        Self { devname, device_type, mode }
    }
}

pub struct UEventFsNode {
    device: Device,
}

impl UEventFsNode {
    pub fn new(device: Device) -> Self {
        Self { device }
    }
}

impl FsNodeOps for UEventFsNode {
    fs_node_impl_not_dir!();

    fn create_file_ops(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _node: &FsNode,
        _current_task: &CurrentTask,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(UEventFile::new(self.device.clone())))
    }
}

struct UEventFile {
    device: Device,
}

impl UEventFile {
    pub fn new(device: Device) -> Self {
        Self { device }
    }

    fn parse_commands(data: &[u8]) -> Vec<&[u8]> {
        data.split(|&c| c == b'\0' || c == b'\n').collect()
    }
}

impl FileOps for UEventFile {
    fileops_impl_seekable!();
    fileops_impl_noop_sync!();

    fn read(
        &self,
        _locked: &mut Locked<FileOpsCore>,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        let content = if let Some(metadata) = &self.device.metadata {
            format!(
                "MAJOR={}\nMINOR={}\nDEVNAME={}\n",
                metadata.device_type.major(),
                metadata.device_type.minor(),
                metadata.devname,
            )
        } else {
            String::new()
        };
        data.write(content.get(offset..).ok_or_else(|| errno!(EINVAL))?.as_bytes())
    }

    fn write(
        &self,
        locked: &mut Locked<FileOpsCore>,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        if offset != 0 {
            return error!(EINVAL);
        }
        let content = data.read_all()?;
        for command in Self::parse_commands(&content) {
            // Ignore empty lines.
            if command == b"" {
                continue;
            }

            match UEventAction::try_from(command) {
                Ok(c) => current_task.kernel().device_registry.dispatch_uevent(
                    locked,
                    c,
                    self.device.clone(),
                ),
                Err(e) => {
                    track_stub!(TODO("https://fxbug.dev/297435061"), "synthetic uevent variables");
                    return Err(e);
                }
            }
        }
        Ok(content.len())
    }
}

#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub enum UEventAction {
    Add,
    Remove,
    Change,
    Move,
    Online,
    Offline,
    Bind,
    Unbind,
}

impl std::fmt::Display for UEventAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UEventAction::Add => write!(f, "add"),
            UEventAction::Remove => write!(f, "remove"),
            UEventAction::Change => write!(f, "change"),
            UEventAction::Move => write!(f, "move"),
            UEventAction::Online => write!(f, "online"),
            UEventAction::Offline => write!(f, "offline"),
            UEventAction::Bind => write!(f, "bind"),
            UEventAction::Unbind => write!(f, "unbind"),
        }
    }
}

impl TryFrom<&[u8]> for UEventAction {
    type Error = Errno;

    fn try_from(action: &[u8]) -> Result<Self, Self::Error> {
        match action {
            b"add" => Ok(UEventAction::Add),
            b"remove" => Ok(UEventAction::Remove),
            b"change" => Ok(UEventAction::Change),
            b"move" => Ok(UEventAction::Move),
            b"online" => Ok(UEventAction::Online),
            b"offline" => Ok(UEventAction::Offline),
            b"bind" => Ok(UEventAction::Bind),
            b"unbind" => Ok(UEventAction::Unbind),
            _ => error!(EINVAL),
        }
    }
}

#[derive(Copy, Clone)]
pub struct UEventContext {
    pub seqnum: u64,
}
