// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::loopback::create_loop_device,
    device::mem::MemDevice,
    device::misc::create_misc_device,
    fs::{kobject::*, FileOps, FsNode},
    lock::RwLock,
    logging::log_error,
    task::*,
    types::*,
};

use std::{
    collections::btree_map::{BTreeMap, Entry},
    marker::{Send, Sync},
    sync::Arc,
};

/// The mode or category of the device driver.
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug)]
pub enum DeviceMode {
    Char,
    Block,
}

pub trait DeviceOps: Send + Sync + 'static {
    fn open(
        &self,
        _current_task: &CurrentTask,
        _id: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno>;
}

/// Allows directly using a function or closure as an implementation of DeviceOps, avoiding having
/// to write a zero-size struct and an impl for it.
impl<F> DeviceOps for F
where
    F: Send
        + Sync
        + Fn(&CurrentTask, DeviceType, &FsNode, OpenFlags) -> Result<Box<dyn FileOps>, Errno>
        + 'static,
{
    fn open(
        &self,
        current_task: &CurrentTask,
        id: DeviceType,
        node: &FsNode,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        self(current_task, id, node, flags)
    }
}

/// Keys returned by the registration method for `DeviceListener`s that allows to unregister a
/// listener.
pub type DeviceListenerKey = u64;

/// A listener for uevents on devices.
pub trait DeviceListener: Send + Sync {
    fn on_device_event(&self, action: UEventAction, kobject: KObjectHandle, context: UEventContext);
}

#[derive(Default)]
struct MajorDevices {
    /// Maps device major number to device implementation.
    map: BTreeMap<u32, Box<dyn DeviceOps>>,
}

impl MajorDevices {
    fn register(&mut self, device: impl DeviceOps, major: u32) -> Result<(), Errno> {
        match self.map.entry(major) {
            Entry::Vacant(e) => {
                e.insert(Box::new(device));
                Ok(())
            }
            Entry::Occupied(_) => {
                log_error!("major device {:?} is already registered", major);
                error!(EINVAL)
            }
        }
    }

    #[allow(clippy::borrowed_box)]
    fn get(&self, id: DeviceType) -> Result<&Box<dyn DeviceOps>, Errno> {
        self.map.get(&id.major()).ok_or_else(|| errno!(ENODEV))
    }
}

/// The kernel's registry of drivers.
pub struct DeviceRegistry {
    root_kobject: KObjectHandle,
    char_devices: MajorDevices,
    block_devices: MajorDevices,
    dyn_devices: Arc<RwLock<DynRegistry>>,
    next_anon_minor: u32,
    listeners: BTreeMap<u64, Box<dyn DeviceListener>>,
    next_listener_id: u64,
    next_event_id: u64,
}

impl DeviceRegistry {
    /// Creates a `DeviceRegistry` and populates it with common drivers such as /dev/null.
    pub fn new_with_common_devices() -> Self {
        let mut registry: DeviceRegistry = Self::default();
        registry.register_chrdev_major(MemDevice, MEM_MAJOR).unwrap();
        registry.register_chrdev_major(create_misc_device, MISC_MAJOR).unwrap();
        registry.register_blkdev_major(create_loop_device, LOOP_MAJOR).unwrap();
        registry.add_common_devices();
        registry
    }

    /// Returns the root kobject.
    pub fn root_kobject(&self) -> KObjectHandle {
        self.root_kobject.clone()
    }

    /// Returns the virtual bus kobject where all virtual and pseudo devices are stored.
    pub fn virtual_bus(&self) -> KObjectHandle {
        self.root_kobject.get_or_create_child(b"virtual", KType::Bus)
    }

    /// Adds a single device kobject in the tree.
    pub fn add_device(&mut self, class: KObjectHandle, dev_attr: KObjectDeviceAttribute) {
        let ktype =
            KType::Device { name: Some(dev_attr.device_name), device_type: dev_attr.device_type };
        self.dispatch_uevent(
            UEventAction::Add,
            class.get_or_create_child(&dev_attr.kobject_name, ktype),
        );
    }

    /// Adds a list of device kobjects in the tree.
    pub fn add_devices(&mut self, class: KObjectHandle, dev_attrs: Vec<KObjectDeviceAttribute>) {
        for attr in dev_attrs {
            self.add_device(class.clone(), attr);
        }
    }

    // TODO(fxb/119437): Refactor how to register a device.
    fn add_common_devices(&mut self) {
        let virtual_bus = self.virtual_bus();

        // MEM class.
        self.add_devices(
            virtual_bus.get_or_create_child(b"mem", KType::Class),
            KObjectDeviceAttribute::new_from_vec(vec![
                (b"null", b"null", DeviceType::NULL),
                (b"zero", b"zero", DeviceType::ZERO),
                (b"full", b"full", DeviceType::FULL),
                (b"random", b"random", DeviceType::RANDOM),
                (b"urandom", b"urandom", DeviceType::URANDOM),
                (b"kmsg", b"kmsg", DeviceType::KMSG),
            ]),
        );

        // MISC class.
        self.add_devices(
            virtual_bus.get_or_create_child(b"misc", KType::Class),
            KObjectDeviceAttribute::new_from_vec(vec![
                (b"hwrng", b"hwrng", DeviceType::HW_RANDOM),
                (b"fuse", b"fuse", DeviceType::FUSE),
                (b"device-mapper", b"mapper/control", DeviceType::DEVICE_MAPPER),
            ]),
        );

        // TTY class.
        self.add_devices(
            virtual_bus.get_or_create_child(b"tty", KType::Class),
            KObjectDeviceAttribute::new_from_vec(vec![
                (b"tty", b"tty", DeviceType::TTY),
                (b"ptmx", b"ptmx", DeviceType::PTMX),
            ]),
        );
    }

    pub fn register_chrdev_major(
        &mut self,
        device: impl DeviceOps,
        major: u32,
    ) -> Result<(), Errno> {
        self.char_devices.register(device, major)
    }

    pub fn register_blkdev_major(
        &mut self,
        device: impl DeviceOps,
        major: u32,
    ) -> Result<(), Errno> {
        self.block_devices.register(device, major)
    }

    pub fn register_dyn_chrdev(&mut self, device: impl DeviceOps) -> Result<DeviceType, Errno> {
        self.dyn_devices.write().register(device)
    }

    pub fn next_anonymous_dev_id(&mut self) -> DeviceType {
        let id = DeviceType::new(0, self.next_anon_minor);
        self.next_anon_minor += 1;
        id
    }

    /// Register a new listener for uevents on devices.
    ///
    /// Returns a key used to unregister the listener.
    pub fn register_listener(
        &mut self,
        listener: impl DeviceListener + 'static,
    ) -> DeviceListenerKey {
        let key = self.next_listener_id;
        self.next_listener_id += 1;
        self.listeners.insert(key, Box::new(listener));
        key
    }

    /// Unregister a listener previously registered through `register_listener`.
    pub fn unregister_listener(&mut self, key: &DeviceListenerKey) {
        self.listeners.remove(key);
    }

    /// Dispatch an uevent for the given `device`.
    /// TODO(fxb/119437): move uevent to sysfs.
    pub fn dispatch_uevent(&mut self, action: UEventAction, kobject: KObjectHandle) {
        let event_id = self.next_event_id;
        self.next_event_id += 1;
        let context = UEventContext { seqnum: event_id };
        for listener in self.listeners.values() {
            listener.on_device_event(action, kobject.clone(), context);
        }
    }

    /// Opens a device file corresponding to the device identifier `dev`.
    pub fn open_device(
        &self,
        current_task: &CurrentTask,
        node: &FsNode,
        flags: OpenFlags,
        id: DeviceType,
        mode: DeviceMode,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let devices = match mode {
            DeviceMode::Char => &self.char_devices,
            DeviceMode::Block => &self.block_devices,
        };
        devices.get(id)?.open(current_task, id, node, flags)
    }
}

impl Default for DeviceRegistry {
    fn default() -> Self {
        let mut registry = Self {
            root_kobject: KObject::new_root(),
            char_devices: Default::default(),
            block_devices: Default::default(),
            dyn_devices: Default::default(),
            next_anon_minor: 1,
            listeners: Default::default(),
            next_listener_id: 0,
            next_event_id: 0,
        };
        registry.char_devices.map.insert(DYN_MAJOR, Box::new(Arc::clone(&registry.dyn_devices)));
        registry
    }
}

#[derive(Default)]
struct DynRegistry {
    dyn_devices: BTreeMap<u32, Box<dyn DeviceOps>>,
    next_dynamic_minor: u32,
}

impl DynRegistry {
    fn register(&mut self, device: impl DeviceOps) -> Result<DeviceType, Errno> {
        // TODO(quiche): Emit a `uevent` to notify userspace of the new device.

        let minor = self.next_dynamic_minor;
        if minor > 255 {
            return error!(ENOMEM);
        }
        self.next_dynamic_minor += 1;
        self.dyn_devices.insert(minor, Box::new(device));
        Ok(DeviceType::new(DYN_MAJOR, minor))
    }
}

impl DeviceOps for Arc<RwLock<DynRegistry>> {
    fn open(
        &self,
        current_task: &CurrentTask,
        id: DeviceType,
        node: &FsNode,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let state = self.read();
        let device = state.dyn_devices.get(&id.minor()).ok_or_else(|| errno!(ENODEV))?;
        device.open(current_task, id, node, flags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{fs::*, testing::*};

    #[::fuchsia::test]
    fn registry_fails_to_add_duplicate_device() {
        let mut registry = DeviceRegistry::default();
        registry.register_chrdev_major(MemDevice, MEM_MAJOR).expect("registers once");
        registry.register_chrdev_major(MemDevice, 123).expect("registers unique");
        registry
            .register_chrdev_major(MemDevice, MEM_MAJOR)
            .expect_err("fail to register duplicate");
    }

    #[::fuchsia::test]
    async fn registry_opens_device() {
        let (_kernel, current_task) = create_kernel_and_task();

        let mut registry = DeviceRegistry::default();
        registry.register_chrdev_major(MemDevice, MEM_MAJOR).unwrap();

        let node = FsNode::new_root(PanickingFsNode);

        // Fail to open non-existent device.
        assert!(registry
            .open_device(
                &current_task,
                &node,
                OpenFlags::RDONLY,
                DeviceType::NONE,
                DeviceMode::Char
            )
            .is_err());

        // Fail to open in wrong mode.
        assert!(registry
            .open_device(
                &current_task,
                &node,
                OpenFlags::RDONLY,
                DeviceType::NULL,
                DeviceMode::Block
            )
            .is_err());

        // Open in correct mode.
        let _ = registry
            .open_device(
                &current_task,
                &node,
                OpenFlags::RDONLY,
                DeviceType::NULL,
                DeviceMode::Char,
            )
            .expect("opens device");
    }

    #[::fuchsia::test]
    async fn test_dynamic_misc() {
        let (_kernel, current_task) = create_kernel_and_task();

        struct TestDevice;
        impl DeviceOps for TestDevice {
            fn open(
                &self,
                _current_task: &CurrentTask,
                _id: DeviceType,
                _node: &FsNode,
                _flags: OpenFlags,
            ) -> Result<Box<dyn FileOps>, Errno> {
                Ok(Box::new(PanickingFile))
            }
        }

        let mut registry = DeviceRegistry::default();
        let device_type = registry.register_dyn_chrdev(TestDevice).unwrap();
        assert_eq!(device_type.major(), DYN_MAJOR);

        let node = FsNode::new_root(PanickingFsNode);
        let _ = registry
            .open_device(&current_task, &node, OpenFlags::RDONLY, device_type, DeviceMode::Char)
            .expect("opens device");
    }
}
