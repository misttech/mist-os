// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::DeviceMode;
use crate::fs::sysfs::KObjectDirectory;
use crate::task::CurrentTask;
use crate::vfs::buffers::{InputBuffer, OutputBuffer};
use crate::vfs::{
    fileops_impl_noop_sync, fileops_impl_seekable, fs_node_impl_not_dir, FileObject, FileOps,
    FsNode, FsNodeOps, FsStr, FsString, PathBuilder,
};
use starnix_logging::track_stub;
use starnix_sync::{FileOpsCore, Locked, Mutex};
use starnix_uapi::device_type::DeviceType;
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;
use starnix_uapi::{errno, error};
use std::collections::BTreeMap;
use std::sync::{Arc, Weak};

/// A kobject is the fundamental unit of the sysfs /devices subsystem. Each kobject represents a
/// sysfs object.
///
/// A kobject has a name, a function to create FsNodeOps, pointers to its children, and a pointer
/// to its parent, which allows it to be organized into hierarchies.
pub struct KObject {
    /// The name that will appear in sysfs.
    ///
    /// It is also used by the parent to find this child. This name will be reflected in the full
    /// path from the root.
    name: FsString,

    /// The weak reference to its parent kobject.
    parent: Option<Weak<KObject>>,

    /// A collection of the children of this kobject.
    ///
    /// The kobject tree has strong references from parent-to-child and weak
    /// references from child-to-parent. This will avoid reference cycle.
    children: Mutex<BTreeMap<FsString, KObjectHandle>>,

    /// Function to create the associated `FsNodeOps`.
    create_fs_node_ops: CreateFsNodeOps,
}
pub type KObjectHandle = Arc<KObject>;

type CreateFsNodeOps = Box<dyn Fn(Weak<KObject>) -> Box<dyn FsNodeOps> + Send + Sync>;

impl KObject {
    pub fn new_root(name: &FsStr) -> KObjectHandle {
        Self::new_root_with_dir(name, KObjectDirectory::new)
    }

    pub fn new_root_with_dir<F, N>(name: &FsStr, create_fs_node_ops: F) -> KObjectHandle
    where
        F: Fn(Weak<KObject>) -> N + Send + Sync + 'static,
        N: FsNodeOps,
    {
        Arc::new(Self {
            name: name.to_owned(),
            parent: None,
            children: Default::default(),
            create_fs_node_ops: Box::new(move |kobject| Box::new(create_fs_node_ops(kobject))),
        })
    }

    fn new_child<F, N>(name: &FsStr, parent: KObjectHandle, create_fs_node_ops: F) -> KObjectHandle
    where
        F: Fn(Weak<KObject>) -> N + Send + Sync + 'static,
        N: FsNodeOps,
    {
        Arc::new(Self {
            name: name.to_owned(),
            parent: Some(Arc::downgrade(&parent)),
            children: Default::default(),
            create_fs_node_ops: Box::new(move |kobject| Box::new(create_fs_node_ops(kobject))),
        })
    }

    /// The name that will appear in sysfs.
    pub fn name(&self) -> &FsStr {
        self.name.as_ref()
    }

    /// The parent kobject.
    ///
    /// Returns none if this kobject is the root.
    pub fn parent(&self) -> Option<KObjectHandle> {
        self.parent.clone().and_then(|parent| Weak::upgrade(&parent))
    }

    /// Returns the associated `FsNodeOps`.
    ///
    /// The `create_fs_node_ops` function will be called with a weak pointer to kobject itself.
    pub fn ops(self: &KObjectHandle) -> Box<dyn FsNodeOps> {
        self.create_fs_node_ops.as_ref()(Arc::downgrade(self))
    }

    /// Get the path to the current kobject, relative to the root.
    pub fn path(self: &KObjectHandle) -> FsString {
        let mut current = Some(self.clone());
        let mut path = PathBuilder::new();
        while let Some(n) = current {
            path.prepend_element(n.name());
            current = n.parent();
        }

        path.build_relative()
    }

    /// Get the path to the root, relative to the current kobject.
    pub fn path_to_root(self: &KObjectHandle) -> FsString {
        let mut parent = self.parent();
        let mut path = PathBuilder::new();
        while let Some(n) = parent {
            path.prepend_element("..".into());
            parent = n.parent();
        }

        path.build_relative()
    }

    /// Checks if there is any child holding the `name`.
    pub fn has_child(self: &KObjectHandle, name: &FsStr) -> bool {
        self.get_child(name).is_some()
    }

    /// Get the child based on the name.
    pub fn get_child(self: &KObjectHandle, name: &FsStr) -> Option<KObjectHandle> {
        self.children.lock().get(name).cloned()
    }

    /// Gets the child if exists, creates a new child if not.
    pub fn get_or_create_child<F, N>(
        self: &KObjectHandle,
        name: &FsStr,
        create_fs_node_ops: F,
    ) -> KObjectHandle
    where
        F: Fn(Weak<KObject>) -> N + Send + Sync + 'static,
        N: FsNodeOps,
    {
        let mut children = self.children.lock();
        match children.get(name).cloned() {
            Some(child) => child,
            None => {
                let child = KObject::new_child(name, self.clone(), create_fs_node_ops);
                children.insert(name.into(), child.clone());
                child
            }
        }
    }

    pub fn insert_child(self: &KObjectHandle, child: KObjectHandle) {
        self.insert_child_with_name(child.name().to_owned(), child);
    }

    pub fn insert_child_with_name(self: &KObjectHandle, name: FsString, child: KObjectHandle) {
        self.children.lock().insert(name, child);
    }

    /// Collects all children names.
    pub fn get_children_names(&self) -> Vec<FsString> {
        self.children.lock().keys().cloned().collect()
    }

    pub fn get_children_kobjects(&self) -> Vec<KObjectHandle> {
        self.children.lock().values().cloned().collect::<Vec<KObjectHandle>>()
    }

    /// Removes the child if exists.
    pub fn remove_child(self: &KObjectHandle, name: &FsStr) -> Option<(FsString, KObjectHandle)> {
        self.children.lock().remove_entry(name)
    }

    /// Removes itself from the parent kobject.
    pub fn remove(&self) {
        if let Some(parent) = self.parent() {
            parent.remove_child(&self.name());
        }
    }
}

impl std::fmt::Debug for KObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KObject").field("name", &self.name()).finish()
    }
}

/// A trait implemented by all kobject-based types.
pub trait KObjectBased {
    fn kobject(&self) -> KObjectHandle;
}

/// Implements the KObjectBased traits for a KObject new type struct.
macro_rules! impl_kobject_based {
    ($type_name:path) => {
        impl KObjectBased for $type_name {
            fn kobject(&self) -> KObjectHandle {
                self.kobject.upgrade().expect("Embedded kobject has been droppped.")
            }
        }
    };
}

/// A collection of devices whose `parent` kobject is not the embedded kobject.
///
/// Used for grouping devices in the sysfs subsystem.
#[derive(Clone, Debug)]
pub struct Collection {
    kobject: Weak<KObject>,
}
impl_kobject_based!(Collection);

impl Collection {
    pub fn new(kobject: KObjectHandle) -> Self {
        Self { kobject: Arc::downgrade(&kobject) }
    }
}

/// A Class is a higher-level view of a device.
///
/// It groups devices based on what they do, rather than how they are connected.
#[derive(Clone, Debug)]
pub struct Class {
    kobject: Weak<KObject>,
    /// Physical bus that the devices belong to.
    pub bus: Bus,
    pub collection: Collection,
}
impl_kobject_based!(Class);

impl Class {
    pub fn new(kobject: KObjectHandle, bus: Bus, collection: Collection) -> Self {
        Self { kobject: Arc::downgrade(&kobject), bus, collection }
    }
}

/// A Bus identifies how the devices are connected to the processor.
#[derive(Clone, Debug)]
pub struct Bus {
    kobject: Weak<KObject>,
    pub collection: Option<Collection>,
}
impl_kobject_based!(Bus);

impl Bus {
    pub fn new(kobject: KObjectHandle, collection: Option<Collection>) -> Self {
        Self { kobject: Arc::downgrade(&kobject), collection }
    }
}

#[derive(Clone, Debug)]
pub struct Device {
    pub kobject: Weak<KObject>,
    /// Class kobject that the device belongs to.
    pub class: Class,
    pub metadata: Option<DeviceMetadata>,
}
impl_kobject_based!(Device);

impl Device {
    pub fn new(kobject: KObjectHandle, class: Class, metadata: Option<DeviceMetadata>) -> Self {
        Self { kobject: Arc::downgrade(&kobject), class, metadata }
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
        _locked: &mut Locked<'_, FileOpsCore>,
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
        _locked: &mut Locked<'_, FileOpsCore>,
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
        _locked: &mut Locked<'_, FileOpsCore>,
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
                Ok(c) => {
                    current_task.kernel().device_registry.dispatch_uevent(c, self.device.clone())
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[::fuchsia::test]
    fn kobject_create_child() {
        let root = KObject::new_root(Default::default());
        assert!(root.parent().is_none());

        assert!(!root.has_child("virtual".into()));
        root.get_or_create_child("virtual".into(), KObjectDirectory::new);
        assert!(root.has_child("virtual".into()));
    }

    #[::fuchsia::test]
    fn kobject_path() {
        let root = KObject::new_root("devices".into());
        let bus = root.get_or_create_child("virtual".into(), KObjectDirectory::new);
        let device = bus
            .get_or_create_child("mem".into(), KObjectDirectory::new)
            .get_or_create_child("null".into(), KObjectDirectory::new);
        assert_eq!(device.path(), "devices/virtual/mem/null");
    }

    #[::fuchsia::test]
    fn kobject_path_to_root() {
        let root = KObject::new_root(Default::default());
        let bus = root.get_or_create_child("bus".into(), KObjectDirectory::new);
        let device = bus.get_or_create_child("device".into(), KObjectDirectory::new);
        assert_eq!(device.path_to_root(), "../..");
    }

    #[::fuchsia::test]
    fn kobject_get_children_names() {
        let root = KObject::new_root(Default::default());
        root.get_or_create_child("virtual".into(), KObjectDirectory::new);
        root.get_or_create_child("cpu".into(), KObjectDirectory::new);
        root.get_or_create_child("power".into(), KObjectDirectory::new);

        let names = root.get_children_names();
        assert!(names.iter().any(|name| *name == "virtual"));
        assert!(names.iter().any(|name| *name == "cpu"));
        assert!(names.iter().any(|name| *name == "power"));
        assert!(!names.iter().any(|name| *name == "system"));
    }

    #[::fuchsia::test]
    fn kobject_remove() {
        let root = KObject::new_root(Default::default());
        let bus = root.get_or_create_child("virtual".into(), KObjectDirectory::new);
        let class = bus.get_or_create_child("mem".into(), KObjectDirectory::new);
        assert!(bus.has_child("mem".into()));
        class.remove();
        assert!(!bus.has_child("mem".into()));
    }
}
