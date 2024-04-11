// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    device::kobject::{
        Bus, Class, Device, DeviceMetadata, KObject, KObjectBased, KObjectHandle, UEventAction,
        UEventContext,
    },
    fs::{
        devtmpfs::{devtmpfs_create_device, devtmpfs_remove_node},
        sysfs::{
            BusCollectionDirectory, ClassCollectionDirectory, DeviceSysfsOps, SysfsDirectory,
            SYSFS_BLOCK, SYSFS_BUS, SYSFS_CLASS, SYSFS_DEVICES,
        },
    },
    task::CurrentTask,
    vfs::{FileOps, FsNode, FsStr},
};
use starnix_logging::{log_error, track_stub};
use starnix_uapi::{
    device_type::{DeviceType, DYN_MAJOR},
    errno, error,
    errors::Errno,
    open_flags::OpenFlags,
};

use starnix_sync::{
    DeviceOpen, FileOpsCore, LockBefore, Locked, MappedMutexGuard, Mutex, MutexGuard, RwLock,
};
use std::{
    collections::btree_map::{BTreeMap, Entry},
    ops::Deref,
    sync::Arc,
};

use dyn_clone::{clone_trait_object, DynClone};
use range_map::RangeMap;

use super::kobject::Collection;

const CHRDEV_MINOR_MAX: u32 = 256;
const BLKDEV_MINOR_MAX: u32 = 2u32.pow(20);

/// The mode or category of the device driver.
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug)]
pub enum DeviceMode {
    Char,
    Block,
}

pub trait DeviceOps: DynClone + Send + Sync + 'static {
    fn open(
        &self,
        _locked: &mut Locked<'_, DeviceOpen>,
        _current_task: &CurrentTask,
        _id: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno>;
}

impl<T: DeviceOps> DeviceOps for Arc<T> {
    fn open(
        &self,
        locked: &mut Locked<'_, DeviceOpen>,
        current_task: &CurrentTask,
        id: DeviceType,
        node: &FsNode,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        self.deref().open(locked, current_task, id, node, flags)
    }
}

clone_trait_object!(DeviceOps);

// This is a newtype instead of an alias so we can implement traits for it.
#[derive(Clone)]
struct DeviceOpsHandle(Arc<dyn DeviceOps>);

impl PartialEq for DeviceOpsHandle {
    fn eq(&self, _other: &Self) -> bool {
        false
    }
}
impl Eq for DeviceOpsHandle {}

/// Allows directly using a function or closure as an implementation of DeviceOps, avoiding having
/// to write a zero-size struct and an impl for it.
impl<F> DeviceOps for F
where
    F: Clone
        + Send
        + Sync
        + Clone
        + Fn(
            &mut Locked<'_, DeviceOpen>,
            &CurrentTask,
            DeviceType,
            &FsNode,
            OpenFlags,
        ) -> Result<Box<dyn FileOps>, Errno>
        + 'static,
{
    fn open(
        &self,
        locked: &mut Locked<'_, DeviceOpen>,
        current_task: &CurrentTask,
        id: DeviceType,
        node: &FsNode,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        self(locked, current_task, id, node, flags)
    }
}

/// A simple `DeviceOps` function for any device that implements `FileOps + Default`.
pub fn simple_device_ops<T: Default + FileOps + 'static>(
    _locked: &mut Locked<'_, DeviceOpen>,
    _current_task: &CurrentTask,
    _id: DeviceType,
    _node: &FsNode,
    _flags: OpenFlags,
) -> Result<Box<dyn FileOps>, Errno> {
    Ok(Box::new(T::default()))
}

/// Keys returned by the registration method for `DeviceListener`s that allows to unregister a
/// listener.
pub type DeviceListenerKey = u64;

/// A listener for uevents on devices.
pub trait DeviceListener: Send + Sync {
    fn on_device_event(&self, action: UEventAction, device: Device, context: UEventContext);
}

struct MajorDevices {
    /// Maps device major number to device implementation.
    map: BTreeMap<u32, RangeMap<u32, DeviceOpsHandle>>,
    mode: DeviceMode,
}

impl MajorDevices {
    fn new(mode: DeviceMode) -> Self {
        Self { map: Default::default(), mode }
    }

    /// Register a `DeviceOps` for a range of minors [`base_minor`, `base_minor` + `minor_count`).
    fn register(
        &mut self,
        major: u32,
        base_minor: u32,
        minor_count: u32,
        ops: impl DeviceOps,
    ) -> Result<(), Errno> {
        let range = base_minor..(base_minor + minor_count);
        let minor_map = self.map.entry(major).or_insert(RangeMap::new());
        if minor_map.intersection(range.clone()).count() == 0 {
            minor_map.insert(range, DeviceOpsHandle(Arc::new(ops)));
            Ok(())
        } else {
            log_error!("Range {:?} overlaps with existing entries in the map.", range);
            error!(EINVAL)
        }
    }

    /// Unregister a range of minors [`base_minor`, `base_minor` + `minor_count`).
    fn unregister(&mut self, major: u32, base_minor: u32, minor_count: u32) -> Result<(), Errno> {
        let range = base_minor..(base_minor + minor_count);
        match self.map.entry(major) {
            Entry::Occupied(minor_map) => {
                minor_map.into_mut().remove(&range);
                Ok(())
            }
            Entry::Vacant(_) => {
                log_error!("No major {} entry registered in the map", major);
                error!(EINVAL)
            }
        }
    }

    /// Register a `DeviceOps` for all minor devices in the `major`.
    fn register_major(&mut self, major: u32, ops: impl DeviceOps) -> Result<(), Errno> {
        let minor_count: u32 = match self.mode {
            DeviceMode::Char => CHRDEV_MINOR_MAX,
            DeviceMode::Block => BLKDEV_MINOR_MAX,
        };
        self.register(major, 0, minor_count, ops)
    }

    /// Unregister all minor devices in the `major`.
    fn unregister_major(&mut self, major: u32) -> Result<(), Errno> {
        let minor_count: u32 = match self.mode {
            DeviceMode::Char => CHRDEV_MINOR_MAX,
            DeviceMode::Block => BLKDEV_MINOR_MAX,
        };
        self.unregister(major, 0, minor_count)
    }

    fn get(&self, id: DeviceType) -> Result<Arc<dyn DeviceOps>, Errno> {
        match self.map.get(&id.major()) {
            Some(minor_map) => match minor_map.get(&id.minor()) {
                Some(entry) => Ok(Arc::clone(&entry.1 .0)),
                None => error!(ENODEV),
            },
            None => error!(ENODEV),
        }
    }
}

/// The kernel's registry of drivers.
pub struct DeviceRegistry {
    root_kobject: KObjectHandle,
    class_subsystem_kobject: KObjectHandle,
    block_subsystem_kobject: KObjectHandle,
    bus_subsystem_kobject: KObjectHandle,
    state: Mutex<DeviceRegistryState>,
}

struct DeviceRegistryState {
    char_devices: MajorDevices,
    block_devices: MajorDevices,
    dyn_devices: Arc<RwLock<DynRegistry>>,
    next_anon_minor: u32,
    listeners: BTreeMap<u64, Box<dyn DeviceListener>>,
    next_listener_id: u64,
    next_event_id: u64,
}

impl DeviceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the root kobject.
    pub fn root_kobject(&self) -> KObjectHandle {
        self.root_kobject.clone()
    }

    pub fn bus_subsystem_kobject(&self) -> KObjectHandle {
        self.bus_subsystem_kobject.clone()
    }

    pub fn class_subsystem_kobject(&self) -> KObjectHandle {
        self.class_subsystem_kobject.clone()
    }

    pub fn block_subsystem_kobject(&self) -> KObjectHandle {
        self.block_subsystem_kobject.clone()
    }

    /// Returns the virtual bus kobject where all virtual and pseudo devices are stored.
    pub fn virtual_bus(&self) -> Bus {
        Bus::new(self.root_kobject.get_or_create_child("virtual".into(), SysfsDirectory::new), None)
    }

    pub fn get_or_create_bus(&self, name: &FsStr) -> Bus {
        let collection = Collection::new(
            self.bus_subsystem_kobject.get_or_create_child(name, BusCollectionDirectory::new),
        );
        Bus::new(self.root_kobject.get_or_create_child(name, SysfsDirectory::new), Some(collection))
    }

    pub fn get_or_create_class(&self, name: &FsStr, bus: Bus) -> Class {
        let collection = Collection::new(
            self.class_subsystem_kobject.get_or_create_child(name, ClassCollectionDirectory::new),
        );
        Class::new(bus.kobject().get_or_create_child(name, SysfsDirectory::new), bus, collection)
    }

    pub fn add_device<F, N, L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        name: &FsStr,
        metadata: DeviceMetadata,
        class: Class,
        create_device_sysfs_ops: F,
    ) -> Device
    where
        F: Fn(Device) -> N + Send + Sync + 'static,
        N: DeviceSysfsOps,
        L: LockBefore<FileOpsCore>,
    {
        let class_cloned = class.clone();
        let metadata_cloned = metadata.clone();
        let device_kobject = class.kobject().get_or_create_child(name, move |kobject| {
            create_device_sysfs_ops(Device::new(
                kobject.upgrade().unwrap(),
                class_cloned.clone(),
                metadata_cloned.clone(),
            ))
        });

        // Insert the created device kobject into its subsystems.
        class.collection.kobject().insert_child(device_kobject.clone());
        if metadata.mode == DeviceMode::Block {
            self.block_subsystem_kobject.insert_child(device_kobject.clone());
        }
        if let Some(bus_collection) = &class.bus.collection {
            bus_collection.kobject().insert_child(device_kobject.clone());
        }

        if let Err(err) = devtmpfs_create_device(locked, current_task, metadata.clone()) {
            log_error!("Cannot add device {:?} in devtmpfs ({:?})", metadata, err);
        }

        let device = device_kobject.ops().as_ref().as_any().downcast_ref::<N>().unwrap().device();
        self.dispatch_uevent(UEventAction::Add, device.clone());
        device
    }

    pub fn add_and_register_device<F, N, L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        name: &FsStr,
        metadata: DeviceMetadata,
        class: Class,
        create_device_sysfs_ops: F,
        dev_ops: impl DeviceOps,
    ) -> Device
    where
        F: Fn(Device) -> N + Send + Sync + 'static,
        N: DeviceSysfsOps,
        L: LockBefore<FileOpsCore>,
    {
        if let Err(err) = self.register_device(
            metadata.device_type.major(),
            metadata.device_type.minor(),
            1,
            dev_ops,
            metadata.mode,
        ) {
            log_error!("Cannot register device {:?} ({:?})", metadata, err);
        }
        self.add_device(locked, current_task, name, metadata, class, create_device_sysfs_ops)
    }

    pub fn remove_device<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        device: Device,
    ) where
        L: LockBefore<FileOpsCore>,
    {
        let kobject = device.kobject();
        let name = kobject.name();
        device.class.collection.kobject().remove_child(name);
        if let Some(bus_collection) = &device.class.bus.collection {
            bus_collection.kobject().remove_child(name);
        }
        device.kobject().remove();
        self.dispatch_uevent(UEventAction::Remove, device.clone());

        if let Err(err) = devtmpfs_remove_node(locked, current_task, device.metadata.name.as_ref())
        {
            log_error!("Cannot remove device {:?} ({:?})", device, err);
        }
    }

    fn major_devices(&self, mode: DeviceMode) -> MappedMutexGuard<'_, MajorDevices> {
        MutexGuard::map(self.state.lock(), |state| match mode {
            DeviceMode::Char => &mut state.char_devices,
            DeviceMode::Block => &mut state.block_devices,
        })
    }

    pub fn register_device(
        &self,
        major: u32,
        base_minor: u32,
        minor_count: u32,
        ops: impl DeviceOps,
        mode: DeviceMode,
    ) -> Result<(), Errno> {
        self.major_devices(mode).register(major, base_minor, minor_count, ops)
    }

    pub fn unregister_device(
        &self,
        major: u32,
        base_minor: u32,
        minor_count: u32,
        mode: DeviceMode,
    ) -> Result<(), Errno> {
        self.major_devices(mode).unregister(major, base_minor, minor_count)
    }

    pub fn register_major(
        &self,
        major: u32,
        device: impl DeviceOps,
        mode: DeviceMode,
    ) -> Result<(), Errno> {
        self.major_devices(mode).register_major(major, device)
    }

    pub fn unregister_major(&self, major: u32, mode: DeviceMode) -> Result<(), Errno> {
        self.major_devices(mode).unregister_major(major)
    }

    pub fn register_dyn_chrdev(&self, device: impl DeviceOps) -> Result<DeviceType, Errno> {
        self.state.lock().dyn_devices.write().register(device)
    }

    pub fn next_anonymous_dev_id(&self) -> DeviceType {
        let mut state = self.state.lock();
        let id = DeviceType::new(0, state.next_anon_minor);
        state.next_anon_minor += 1;
        id
    }

    /// Register a new listener for uevents on devices.
    ///
    /// Returns a key used to unregister the listener.
    pub fn register_listener(&self, listener: impl DeviceListener + 'static) -> DeviceListenerKey {
        let mut state = self.state.lock();
        let key = state.next_listener_id;
        state.next_listener_id += 1;
        state.listeners.insert(key, Box::new(listener));
        key
    }

    /// Unregister a listener previously registered through `register_listener`.
    pub fn unregister_listener(&self, key: &DeviceListenerKey) {
        self.state.lock().listeners.remove(key);
    }

    /// Dispatch an uevent for the given `device`.
    pub fn dispatch_uevent(&self, action: UEventAction, device: Device) {
        let mut state = self.state.lock();
        let event_id = state.next_event_id;
        state.next_event_id += 1;
        let context = UEventContext { seqnum: event_id };
        for listener in state.listeners.values() {
            listener.on_device_event(action, device.clone(), context);
        }
    }

    /// Opens a device file corresponding to the device identifier `dev`.
    pub fn open_device<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        node: &FsNode,
        flags: OpenFlags,
        id: DeviceType,
        mode: DeviceMode,
    ) -> Result<Box<dyn FileOps>, Errno>
    where
        L: LockBefore<DeviceOpen>,
    {
        let device_ops = {
            // Access the actual devices in a nested scope to avoid holding the state lock while
            // creating the FileOps from any single DeviceOps.
            let state = self.state.lock();
            let devices = match mode {
                DeviceMode::Char => &state.char_devices,
                DeviceMode::Block => &state.block_devices,
            };
            devices.get(id)?
        };
        let mut locked = locked.cast_locked::<DeviceOpen>();
        device_ops.open(&mut locked, current_task, id, node, flags)
    }
}

impl Default for DeviceRegistry {
    fn default() -> Self {
        let mut state = DeviceRegistryState {
            char_devices: MajorDevices::new(DeviceMode::Char),
            block_devices: MajorDevices::new(DeviceMode::Block),
            dyn_devices: Default::default(),
            next_anon_minor: 1,
            listeners: Default::default(),
            next_listener_id: 0,
            next_event_id: 0,
        };
        state
            .char_devices
            .register_major(DYN_MAJOR, Arc::clone(&state.dyn_devices))
            .expect("Failed to register DYN_MAJOR");
        Self {
            root_kobject: KObject::new_root(SYSFS_DEVICES.into()),
            class_subsystem_kobject: KObject::new_root(SYSFS_CLASS.into()),
            block_subsystem_kobject: KObject::new_root(SYSFS_BLOCK.into()),
            bus_subsystem_kobject: KObject::new_root(SYSFS_BUS.into()),
            state: Mutex::new(state),
        }
    }
}

#[derive(Default)]
struct DynRegistry {
    dyn_devices: BTreeMap<u32, Arc<Box<dyn DeviceOps>>>,
    next_dynamic_minor: u32,
}

impl DynRegistry {
    fn register(&mut self, device: impl DeviceOps) -> Result<DeviceType, Errno> {
        track_stub!(TODO("https://fxbug.dev/322873632"), "emit uevent for dynamic registration");

        let minor = self.next_dynamic_minor;
        if minor > 255 {
            return error!(ENOMEM);
        }
        self.next_dynamic_minor += 1;
        self.dyn_devices.insert(minor, Arc::new(Box::new(device)));
        Ok(DeviceType::new(DYN_MAJOR, minor))
    }
}

impl DeviceOps for Arc<RwLock<DynRegistry>> {
    fn open(
        &self,
        locked: &mut Locked<'_, DeviceOpen>,
        current_task: &CurrentTask,
        id: DeviceType,
        node: &FsNode,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        let device =
            Arc::clone(self.read().dyn_devices.get(&id.minor()).ok_or_else(|| errno!(ENODEV))?);
        device.open(locked, current_task, id, node, flags)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{device::mem::DevNull, fs::sysfs::DeviceDirectory, testing::*, vfs::*};
    use starnix_uapi::device_type::{INPUT_MAJOR, MEM_MAJOR};

    #[::fuchsia::test]
    fn registry_fails_to_add_duplicate_device() {
        let registry = DeviceRegistry::default();
        registry
            .register_major(MEM_MAJOR, simple_device_ops::<DevNull>, DeviceMode::Char)
            .expect("registers once");
        registry
            .register_major(123, simple_device_ops::<DevNull>, DeviceMode::Char)
            .expect("registers unique");
        registry
            .register_major(MEM_MAJOR, simple_device_ops::<DevNull>, DeviceMode::Char)
            .expect_err("fail to register duplicate");
    }

    #[::fuchsia::test]
    async fn registry_opens_device() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        let registry = DeviceRegistry::default();
        registry
            .register_major(MEM_MAJOR, simple_device_ops::<DevNull>, DeviceMode::Char)
            .expect("registers unique");

        let node = FsNode::new_root(PanickingFsNode);

        // Fail to open non-existent device.
        assert!(registry
            .open_device(
                &mut locked,
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
                &mut locked,
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
                &mut locked,
                &current_task,
                &node,
                OpenFlags::RDONLY,
                DeviceType::NULL,
                DeviceMode::Char,
            )
            .expect("opens device");
    }

    #[::fuchsia::test]
    async fn registry_dynamic_misc() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        fn create_test_device(
            _locked: &mut Locked<'_, DeviceOpen>,
            _current_task: &CurrentTask,
            _id: DeviceType,
            _node: &FsNode,
            _flags: OpenFlags,
        ) -> Result<Box<dyn FileOps>, Errno> {
            Ok(Box::new(PanickingFile))
        }

        let registry = DeviceRegistry::default();
        let device_type = registry.register_dyn_chrdev(create_test_device).unwrap();
        assert_eq!(device_type.major(), DYN_MAJOR);

        let node = FsNode::new_root(PanickingFsNode);
        let _ = registry
            .open_device(
                &mut locked,
                &current_task,
                &node,
                OpenFlags::RDONLY,
                device_type,
                DeviceMode::Char,
            )
            .expect("opens device");
    }

    #[::fuchsia::test]
    async fn registery_add_class() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let registry = &kernel.device_registry;

        let input_class = registry.get_or_create_class("input".into(), registry.virtual_bus());
        registry.add_device(
            &mut locked,
            &current_task,
            "mice".into(),
            DeviceMetadata::new("mice".into(), DeviceType::new(INPUT_MAJOR, 0), DeviceMode::Char),
            input_class,
            DeviceDirectory::new,
        );

        assert!(registry.class_subsystem_kobject().has_child("input".into()));
        assert!(registry
            .class_subsystem_kobject()
            .get_child("input".into())
            .and_then(|collection| collection.get_child("mice".into()))
            .is_some());
    }

    #[::fuchsia::test]
    async fn registry_add_bus() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let registry = &kernel.device_registry;

        let bus = registry.get_or_create_bus("bus".into());
        let class = registry.get_or_create_class("class".into(), bus);
        registry.add_device(
            &mut locked,
            &current_task,
            "device".into(),
            DeviceMetadata::new("device".into(), DeviceType::new(0, 0), DeviceMode::Char),
            class,
            DeviceDirectory::new,
        );
        assert!(registry.bus_subsystem_kobject().has_child("bus".into()));
        assert!(registry
            .bus_subsystem_kobject()
            .get_child("bus".into())
            .and_then(|collection| collection.get_child("device".into()))
            .is_some());
    }

    #[::fuchsia::test]
    async fn registry_remove_device() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let registry = &kernel.device_registry;

        let pci_bus = registry.get_or_create_bus("pci".into());
        let input_class = registry.get_or_create_class("input".into(), pci_bus);
        let mice_dev = registry.add_device(
            &mut locked,
            &current_task,
            "mice".into(),
            DeviceMetadata::new("mice".into(), DeviceType::new(INPUT_MAJOR, 0), DeviceMode::Char),
            input_class.clone(),
            DeviceDirectory::new,
        );

        registry.remove_device(&mut locked, &current_task, mice_dev);
        assert!(!input_class.kobject().has_child("mice".into()));
        assert!(!registry
            .bus_subsystem_kobject()
            .get_child("pci".into())
            .expect("get pci collection")
            .has_child("mice".into()));
        assert!(!registry
            .class_subsystem_kobject()
            .get_child("input".into())
            .expect("get input collection")
            .has_child("mice".into()));
    }

    #[::fuchsia::test]
    async fn registery_unregister_device() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let registry = &kernel.device_registry;
        let node = FsNode::new_root(PanickingFsNode);
        let _ = registry
            .open_device(
                &mut locked,
                &current_task,
                &node,
                OpenFlags::RDONLY,
                DeviceType::NULL,
                DeviceMode::Char,
            )
            .expect("opens device");

        let _ = registry.unregister_major(MEM_MAJOR, DeviceMode::Char);
        assert!(registry
            .open_device(
                &mut locked,
                &current_task,
                &node,
                OpenFlags::RDONLY,
                DeviceType::NULL,
                DeviceMode::Char,
            )
            .is_err());
    }
}
