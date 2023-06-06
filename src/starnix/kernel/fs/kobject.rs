// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use super::*;

use std::{
    collections::BTreeMap,
    sync::{Arc, Weak},
};

use crate::{
    fs::buffers::{InputBuffer, OutputBuffer},
    lock::Mutex,
    task::*,
    types::*,
};

#[derive(Debug, PartialEq, Clone)]
pub enum KType {
    /// Root of the kobject tree.
    Root,
    /// A bus which devices can be attached to.
    Bus,
    /// A group of devices that have a smilar behavior.
    Class,
    /// A virtual/physical device that is attached to a bus.
    ///
    /// Contains all information required in the `dev` and `uevent` files.
    Device {
        /// Name of the device in /dev.
        name: Option<FsString>,
        device_type: DeviceType,
    },
}

impl std::fmt::Display for KType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KType::Root => write!(f, "Root"),
            KType::Bus => write!(f, "Bus"),
            KType::Class => write!(f, "Class"),
            KType::Device { name, device_type } => {
                let name = match name {
                    Some(name) => String::from_utf8(name.to_vec()).unwrap(),
                    None => String::from("Unknown"),
                };
                write!(f, "Device (\"{name}\" {device_type})")
            }
        }
    }
}

/// Attributes that are used to create a KType::Device kobject.
pub struct KObjectDeviceAttribute {
    pub kobject_name: FsString,
    pub device_name: FsString,
    pub device_type: DeviceType,
}

impl KObjectDeviceAttribute {
    pub fn new(kobject_name: &FsStr, device_name: &FsStr, device_type: DeviceType) -> Self {
        Self { kobject_name: kobject_name.to_vec(), device_name: device_name.to_vec(), device_type }
    }

    /// Create a list of `KObjectDeviceAttribute`s.
    ///
    /// # Arguments
    /// * `attributes` - A list of tuples that represent the attributes of a kobject device.
    ///                  Each attribute tuple formats as:
    ///                    (kobject_name: &FsStr, device_name: &FsStr, device_type: DeviceType).
    pub fn new_from_vec(attributes: Vec<(&FsStr, &FsStr, DeviceType)>) -> Vec<Self> {
        attributes
            .into_iter()
            .map(|(kobject_name, device_name, device_type)| {
                KObjectDeviceAttribute::new(kobject_name, device_name, device_type)
            })
            .collect()
    }
}

#[derive(Debug)]
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

    /// The type of object that embeds a kobject.
    ///
    /// It controls what happens to the kobject when being created and destroyed.
    ktype: KType,
}
pub type KObjectHandle = Arc<KObject>;

impl KObject {
    pub fn new_root() -> KObjectHandle {
        Arc::new(Self {
            name: Default::default(),
            parent: None,
            children: Default::default(),
            ktype: KType::Root,
        })
    }

    fn new(name: &FsStr, parent: KObjectHandle, ktype: KType) -> KObjectHandle {
        Arc::new(Self {
            name: name.to_vec(),
            parent: Some(Arc::downgrade(&parent)),
            children: Default::default(),
            ktype,
        })
    }

    /// The name that will appear in sysfs.
    pub fn name(&self) -> FsString {
        self.name.clone()
    }

    /// The parent kobject.
    ///
    /// Returns none if this kobject is the root.
    pub fn parent(&self) -> Option<KObjectHandle> {
        self.parent.clone().and_then(|parent| Weak::upgrade(&parent))
    }

    /// The type of object that embeds a kobject.
    pub fn ktype(&self) -> KType {
        self.ktype.clone()
    }

    /// Get the full path from the root.
    pub fn path(self: &KObjectHandle) -> FsString {
        let mut current = Some(self.clone());
        let mut path: Vec<String> = vec![];
        while let Some(n) = current {
            path.push(String::from_utf8(n.name()).unwrap());
            current = n.parent();
        }

        path.reverse();
        path.join("/").into()
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
    pub fn get_or_create_child(self: &KObjectHandle, name: &FsStr, ktype: KType) -> KObjectHandle {
        let mut children = self.children.lock();
        match children.get(name).cloned() {
            Some(child) => child,
            None => {
                let child = KObject::new(name, self.clone(), ktype);
                children.insert(name.to_vec(), child.clone());
                child
            }
        }
    }

    /// Collects all children names.
    pub fn get_children_names(&self) -> Vec<FsString> {
        self.children.lock().keys().cloned().collect()
    }
}

impl std::fmt::Display for KObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "name: \"{}\", ktype: {}", String::from_utf8(self.name()).unwrap(), self.ktype())
    }
}

pub struct UEventFsNode {
    kobject: KObjectHandle,
}

impl UEventFsNode {
    pub fn new(kobject: KObjectHandle) -> Self {
        Self { kobject }
    }
}

impl FsNodeOps for UEventFsNode {
    fn create_file_ops(
        &self,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        Ok(Box::new(UEventFile::new(self.kobject.clone())))
    }
}

struct UEventFile {
    kobject: KObjectHandle,
}

impl UEventFile {
    pub fn new(kobject: KObjectHandle) -> Self {
        Self { kobject }
    }

    fn parse_commands(data: &[u8]) -> Vec<&[u8]> {
        data.split(|&c| c == b'\0' || c == b'\n').collect()
    }
}

impl FileOps for UEventFile {
    fileops_impl_seekable!();

    fn read(
        &self,
        _file: &FileObject,
        _current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn OutputBuffer,
    ) -> Result<usize, Errno> {
        match self.kobject.ktype() {
            KType::Device { name: device_name, device_type } => {
                let mut content =
                    format!("MAJOR={}\nMINOR={}\n", device_type.major(), device_type.minor(),);
                if device_name.is_some() {
                    content +=
                        &format!("DEVNAME={}\n", String::from_utf8_lossy(&device_name.unwrap()));
                }
                data.write(content[offset..].as_bytes())
            }
            _ => error!(ENODEV),
        }
    }

    fn write(
        &self,
        _file: &FileObject,
        current_task: &CurrentTask,
        offset: usize,
        data: &mut dyn InputBuffer,
    ) -> Result<usize, Errno> {
        if offset != 0 {
            return error!(EINVAL);
        }
        // TODO(fxb/127713): Support parsing synthetic variables.
        let content = data.read_all()?;
        for command in Self::parse_commands(&content) {
            // Ignore empty lines.
            if command == b"" {
                continue;
            }

            current_task
                .kernel()
                .device_registry
                .write()
                .dispatch_uevent(command.try_into()?, self.kobject.clone());
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
        let root = KObject::new_root();
        assert!(root.parent().is_none());

        assert!(!root.has_child(b"virtual"));
        root.get_or_create_child(b"virtual", KType::Bus);
        assert!(root.has_child(b"virtual"));
    }

    #[::fuchsia::test]
    fn kobject_path() {
        let root = KObject::new_root();
        let device = root
            .get_or_create_child(b"virtual", KType::Bus)
            .get_or_create_child(b"mem", KType::Class)
            .get_or_create_child(
                b"null",
                KType::Device { name: Some(b"null".to_vec()), device_type: DeviceType::NULL },
            );
        assert_eq!(device.path(), b"/virtual/mem/null".to_vec());
    }

    #[::fuchsia::test]
    fn kobject_get_children_names() {
        let root = KObject::new_root();
        root.get_or_create_child(b"virtual", KType::Bus);
        root.get_or_create_child(b"cpu", KType::Bus);
        root.get_or_create_child(b"power", KType::Bus);

        let names = root.get_children_names();
        assert!(names.iter().any(|name| *name == b"virtual".to_vec()));
        assert!(names.iter().any(|name| *name == b"cpu".to_vec()));
        assert!(names.iter().any(|name| *name == b"power".to_vec()));
        assert!(!names.iter().any(|name| *name == b"system".to_vec()));
    }
}
