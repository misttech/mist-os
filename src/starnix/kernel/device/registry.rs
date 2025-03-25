// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::{Class, Device, DeviceMetadata, UEventAction, UEventContext};
use crate::device::kobject_store::KObjectStore;
use crate::fs::devtmpfs::{devtmpfs_create_device, devtmpfs_remove_node};
use crate::fs::sysfs::DeviceDirectory;
use crate::task::CurrentTask;
use crate::vfs::{FileOps, FsNode, FsNodeOps, FsStr, FsString};
use starnix_logging::{log_error, log_warn};
use starnix_uapi::device_type::{
    DeviceType, DYN_MAJOR_RANGE, MISC_DYNANIC_MINOR_RANGE, MISC_MAJOR,
};
use starnix_uapi::error;
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;

use starnix_sync::{
    DeviceOpen, FileOpsCore, LockBefore, Locked, MappedMutexGuard, Mutex, MutexGuard,
};
use std::collections::btree_map::{BTreeMap, Entry};
use std::ops::{Deref, Range};
use std::sync::Arc;

use dyn_clone::{clone_trait_object, DynClone};

const CHRDEV_MINOR_MAX: u32 = 256;
const BLKDEV_MINOR_MAX: u32 = 2u32.pow(20);

/// The mode or category of the device driver.
#[derive(Copy, Clone, Ord, PartialOrd, Eq, PartialEq, Debug)]
pub enum DeviceMode {
    /// This device is a character device.
    Char,

    /// This device is a block device.
    Block,
}

impl DeviceMode {
    /// The number of minor device numbers available for this device mode.
    fn minor_count(&self) -> u32 {
        match self {
            Self::Char => CHRDEV_MINOR_MAX,
            Self::Block => BLKDEV_MINOR_MAX,
        }
    }

    /// The range of minor device numbers available for this device mode.
    pub fn minor_range(&self) -> Range<u32> {
        0..self.minor_count()
    }
}

pub trait DeviceOps: DynClone + Send + Sync + 'static {
    /// Instantiate a FileOps for this device.
    ///
    /// This function is called when userspace opens a file with a `DeviceType`
    /// assigned to this device.
    fn open(
        &self,
        _locked: &mut Locked<'_, DeviceOpen>,
        _current_task: &CurrentTask,
        _device_type: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno>;
}

impl<T: DeviceOps> DeviceOps for Arc<T> {
    fn open(
        &self,
        locked: &mut Locked<'_, DeviceOpen>,
        current_task: &CurrentTask,
        device_type: DeviceType,
        node: &FsNode,
        flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno> {
        self.deref().open(locked, current_task, device_type, node, flags)
    }
}

clone_trait_object!(DeviceOps);

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

/// An entry in the `DeviceRegistry`.
struct DeviceEntry {
    /// The name of the device.
    ///
    /// This name is the same as the name of the KObject for the device.
    name: FsString,

    /// The ops used to open the device.
    ops: Arc<dyn DeviceOps>,
}

impl DeviceEntry {
    fn new(name: FsString, ops: impl DeviceOps) -> Self {
        Self { name, ops: Arc::new(ops) }
    }
}

/// The devices registered for a given `DeviceMode`.
///
/// Each `DeviceMode` has its own namespace of registered devices.
#[derive(Default)]
struct RegisteredDevices {
    /// The major devices registered for this device mode.
    ///
    /// Typically the devices registered here will add and remove individual devices using the
    /// `add_device` and `remove_device` functions on `DeviceRegistry`.
    ///
    /// A major device registration shadows any minor device registrations for the same major
    /// device number. We might need to reconsider this choice in the future in order to make
    /// the /proc/devices file correctly list major devices such as `misc`.
    majors: BTreeMap<u32, DeviceEntry>,

    /// Individually registered minor devices.
    ///
    /// These devices are registered using the `register_device` function on `DeviceRegistry`.
    minors: BTreeMap<DeviceType, DeviceEntry>,
}

impl RegisteredDevices {
    /// Register a major device.
    ///
    /// Returns `EINVAL` if the major device is already registered.
    fn register_major(&mut self, major: u32, entry: DeviceEntry) -> Result<(), Errno> {
        if let Entry::Vacant(slot) = self.majors.entry(major) {
            slot.insert(entry);
            Ok(())
        } else {
            error!(EINVAL)
        }
    }

    /// Register a minor device.
    ///
    /// Overwrites any existing minor device registered with the given `DeviceType`.
    fn register_minor(&mut self, device_type: DeviceType, entry: DeviceEntry) {
        self.minors.insert(device_type, entry);
    }

    /// Get the ops for a given `DeviceType`.
    ///
    /// If there is a major device registered with the major device number of the
    /// `DeviceType`, the ops for that major device will be returned. Otherwise,
    /// if there is a minor device registered, the ops for that minor device will be
    /// returned. Otherwise, returns `ENODEV`.
    fn get(&self, device_type: DeviceType) -> Result<Arc<dyn DeviceOps>, Errno> {
        if let Some(major_device) = self.majors.get(&device_type.major()) {
            Ok(Arc::clone(&major_device.ops))
        } else if let Some(minor_device) = self.minors.get(&device_type) {
            Ok(Arc::clone(&minor_device.ops))
        } else {
            error!(ENODEV)
        }
    }

    /// Returns a list of the registered major device numbers and their names.
    fn list_major_devices(&self) -> Vec<(u32, FsString)> {
        self.majors.iter().map(|(major, entry)| (*major, entry.name.clone())).collect()
    }

    /// Returns a list of the registered minor devices and their names.
    fn list_minor_devices(&self, range: Range<DeviceType>) -> Vec<(DeviceType, FsString)> {
        self.minors
            .range(range)
            .map(|(device_type, entry)| (device_type.clone(), entry.name.clone()))
            .collect()
    }
}

/// The registry for devices.
///
/// Devices are specified in file systems with major and minor device numbers, together referred to
/// as a `DeviceType`. When userspace opens one of those files, we look up the `DeviceType` in the
/// device registry to instantiate a file for that device.
///
/// The `DeviceRegistry` also manages the `KObjectStore`, which provides metadata for devices via
/// the sysfs file system, typically mounted at /sys.
pub struct DeviceRegistry {
    /// The KObjects for registered devices.
    pub objects: KObjectStore,

    /// Mutable state for the device registry.
    state: Mutex<DeviceRegistryState>,
}
struct DeviceRegistryState {
    /// The registered character devices.
    char_devices: RegisteredDevices,

    /// The registered block devices.
    block_devices: RegisteredDevices,

    /// Some of the misc devices (devices with the `MISC_MAJOR` major number) are dynamically
    /// allocated. This allocator keeps track of which device numbers have been allocated to
    /// such devices.
    misc_chardev_allocator: DeviceTypeAllocator,

    /// A range of large major device numbers are reserved for other dynamically allocated
    /// devices. This allocator keeps track of which device numbers have been allocated to
    /// such devices.
    dyn_chardev_allocator: DeviceTypeAllocator,

    /// The next anonymous device number to assign to a file system.
    next_anon_minor: u32,

    /// Listeners registered to learn about new devices being added to the registry.
    ///
    /// These listeners generate uevents for those devices, which populates /dev on some
    /// systems.
    listeners: BTreeMap<u64, Box<dyn DeviceListener>>,

    /// The next identifier to use for a listener.
    next_listener_id: u64,

    /// The next event identifier to use when notifying listeners.
    next_event_id: u64,
}

impl DeviceRegistry {
    /// Notify devfs and listeners that a device has been added to the registry.
    fn notify_device<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        device: Device,
    ) where
        L: LockBefore<FileOpsCore>,
    {
        if let Some(metadata) = &device.metadata {
            if let Err(err) = devtmpfs_create_device(locked, current_task, metadata.clone()) {
                log_warn!("Cannot add device {:?} in devtmpfs ({:?})", metadata, err);
            }
            self.dispatch_uevent(UEventAction::Add, device);
        }
    }

    /// Register a device with the `DeviceRegistry`.
    ///
    /// If you are registering a device that exists in other systems, please check the metadata
    /// for that device in /sys and make sure you use the same properties when calling this
    /// function because these value are visible to userspace.
    ///
    /// For example, a typical device will appear in sysfs at a path like:
    ///
    ///   `/sys/devices/{bus}/{class}/{name}`
    ///
    /// Many common classes have convenient accessors on `DeviceRegistry::objects`.
    ///
    /// To fill out the `DeviceMetadata`, look at the `uevent` file:
    ///
    ///   `/sys/devices/{bus}/{class}/{name}/uevent`
    ///
    /// which as the following format:
    ///
    /// ```
    ///   MAJOR={major-number}
    ///   MINOR={minor-number}
    ///   DEVNAME={devname}
    ///   DEVMODE={devmode}
    /// ```
    ///
    /// Often, the `{name}` and the `{devname}` are the same, but if they are not the same,
    /// please take care to use the correct string in the correct field.
    ///
    /// If the `{major-number}` is 10 and the `{minor-number}` is in the range 52..128, please use
    /// `register_misc_device` instead because these device numbers are dynamically allocated.
    ///
    /// If the `{major-number}` is in the range 234..255, please use `register_dyn_device` instead
    /// because these device are also dynamically allocated.
    ///
    /// If you are unsure which device numbers to use, consult devices.txt:
    ///
    ///   https://www.kernel.org/doc/Documentation/admin-guide/devices.txt
    ///
    /// If you are still unsure, please ask an experienced Starnix contributor rather than make up
    /// a device number.
    ///
    /// For most devices, the `create_device_sysfs_ops` parameter should be
    /// `DeviceDirectory::new`, but some devices have custom directories in sysfs.
    ///
    /// Finally, the `dev_ops` parameter is where you provide the callback for instantiating
    /// your device.
    pub fn register_device<F, N, L>(
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
        N: FsNodeOps,
        L: LockBefore<FileOpsCore>,
    {
        let entry = DeviceEntry::new(name.into(), dev_ops);
        self.devices(metadata.mode).register_minor(metadata.device_type, entry);
        let device =
            self.objects.create_device(name, Some(metadata), class, create_device_sysfs_ops);
        self.notify_device(locked, current_task, device.clone());
        device
    }

    /// Register a dynamic device in the `MISC_MAJOR` major device number.
    ///
    /// MISC devices (major number 10) with minor numbers in the range 52..128 are dynamically
    /// assigned. Rather than hardcoding registrations with these device numbers, use this
    /// function instead to register the device.
    ///
    /// See `register_device` for an explanation of the parameters.
    pub fn register_misc_device<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        name: &FsStr,
        dev_ops: impl DeviceOps,
    ) -> Result<Device, Errno>
    where
        L: LockBefore<FileOpsCore>,
    {
        let device_type = self.state.lock().misc_chardev_allocator.allocate()?;
        let metadata = DeviceMetadata::new(name.into(), device_type, DeviceMode::Char);
        Ok(self.register_device(
            locked,
            current_task,
            name,
            metadata,
            self.objects.misc_class(),
            DeviceDirectory::new,
            dev_ops,
        ))
    }

    /// Register a dynamic device with major numbers 234..255.
    ///
    /// Majors device numbers 234..255 are dynamically assigned. Rather than hardcoding
    /// registrations with these device numbers, use this function instead to register the device.
    ///
    /// Note: We do not currently allocate from this entire range because we have mistakenly
    /// hardcoded some device registrations from the dynamic range. Once we fix these registrations
    /// to be dynamic, we should expand to using the full dynamic range.
    ///
    /// See `register_device` for an explanation of the parameters.
    pub fn register_dyn_device<F, N, L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        name: &FsStr,
        class: Class,
        create_device_sysfs_ops: F,
        dev_ops: impl DeviceOps,
    ) -> Result<Device, Errno>
    where
        F: Fn(Device) -> N + Send + Sync + 'static,
        N: FsNodeOps,
        L: LockBefore<FileOpsCore>,
    {
        let device_type = self.state.lock().dyn_chardev_allocator.allocate()?;
        let metadata = DeviceMetadata::new(name.into(), device_type, DeviceMode::Char);
        Ok(self.register_device(
            locked,
            current_task,
            name,
            metadata,
            class,
            create_device_sysfs_ops,
            dev_ops,
        ))
    }

    /// Directly add a device to the KObjectStore.
    ///
    /// This function should be used only by device that have registered an entire major device
    /// number. If you want to add a single minor device, use the `register_device` function
    /// instead.
    ///
    /// See `register_device` for an explanation of the parameters.
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
        N: FsNodeOps,
        L: LockBefore<FileOpsCore>,
    {
        self.devices(metadata.mode).get(metadata.device_type).expect("device is registered");
        let device =
            self.objects.create_device(name, Some(metadata), class, create_device_sysfs_ops);
        self.notify_device(locked, current_task, device.clone());
        device
    }

    /// Directly add a device to the KObjectStore that lacks a device number.
    ///
    /// This function should be used only by device do not have a major or a minor number. You can
    /// identify these devices because they appear in sysfs and have an empty `uevent` file.
    ///
    /// See `register_device` for an explanation of the parameters.
    pub fn add_numberless_device<F, N, L>(
        &self,
        _locked: &mut Locked<'_, L>,
        _current_task: &CurrentTask,
        name: &FsStr,
        class: Class,
        create_device_sysfs_ops: F,
    ) -> Device
    where
        F: Fn(Device) -> N + Send + Sync + 'static,
        N: FsNodeOps,
        L: LockBefore<FileOpsCore>,
    {
        self.objects.create_device(name, None, class, create_device_sysfs_ops)
    }

    /// Remove a device directly added with `add_device`.
    ///
    /// This function should be used only by device that have registered an entire major device
    /// number. Individually registered minor device cannot be removed at this time.
    pub fn remove_device<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        device: Device,
    ) where
        L: LockBefore<FileOpsCore>,
    {
        if let Some(metadata) = &device.metadata {
            self.dispatch_uevent(UEventAction::Remove, device.clone());

            if let Err(err) = devtmpfs_remove_node(locked, current_task, metadata.devname.as_ref())
            {
                log_error!("Cannot remove device {:?} ({:?})", device, err);
            }
        }

        self.objects.destroy_device(&device);
    }

    /// Returns a list of the registered major device numbers for the given `DeviceMode` and their
    /// names.
    pub fn list_major_devices(&self, mode: DeviceMode) -> Vec<(u32, FsString)> {
        self.devices(mode).list_major_devices()
    }

    /// Returns a list of the registered minor devices for the given `DeviceMode` and their names.
    pub fn list_minor_devices(
        &self,
        mode: DeviceMode,
        range: Range<DeviceType>,
    ) -> Vec<(DeviceType, FsString)> {
        self.devices(mode).list_minor_devices(range)
    }

    /// The `RegisteredDevice` object for the given `DeviceMode`.
    fn devices(&self, mode: DeviceMode) -> MappedMutexGuard<'_, RegisteredDevices> {
        MutexGuard::map(self.state.lock(), |state| match mode {
            DeviceMode::Char => &mut state.char_devices,
            DeviceMode::Block => &mut state.block_devices,
        })
    }

    /// Register an entire major device number.
    ///
    /// If you register an entire major device, use `add_device` and `remove_device` to manage the
    /// sysfs entiries for your device rather than trying to register and unregister individual
    /// minor devices.
    pub fn register_major(
        &self,
        name: FsString,
        mode: DeviceMode,
        major: u32,
        dev_ops: impl DeviceOps,
    ) -> Result<(), Errno> {
        let entry = DeviceEntry::new(name, dev_ops);
        self.devices(mode).register_major(major, entry)
    }

    /// Allocate an anonymous device identifier.
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

    /// Instantiate a file for the specified device.
    ///
    /// The device will be looked up in the device registry by `DeviceMode` and `DeviceType`.
    pub fn open_device<L>(
        &self,
        locked: &mut Locked<'_, L>,
        current_task: &CurrentTask,
        node: &FsNode,
        flags: OpenFlags,
        device_type: DeviceType,
        mode: DeviceMode,
    ) -> Result<Box<dyn FileOps>, Errno>
    where
        L: LockBefore<DeviceOpen>,
    {
        let dev_ops = self.devices(mode).get(device_type)?;
        let mut locked = locked.cast_locked::<DeviceOpen>();
        dev_ops.open(&mut locked, current_task, device_type, node, flags)
    }
}

impl Default for DeviceRegistry {
    fn default() -> Self {
        let misc_available = vec![DeviceType::new_range(MISC_MAJOR, MISC_DYNANIC_MINOR_RANGE)];
        let dyn_available = DYN_MAJOR_RANGE
            .map(|major| DeviceType::new_range(major, DeviceMode::Char.minor_range()))
            .rev()
            .collect();
        let state = DeviceRegistryState {
            char_devices: Default::default(),
            block_devices: Default::default(),
            misc_chardev_allocator: DeviceTypeAllocator::new(misc_available),
            dyn_chardev_allocator: DeviceTypeAllocator::new(dyn_available),
            next_anon_minor: 1,
            listeners: Default::default(),
            next_listener_id: 0,
            next_event_id: 0,
        };
        Self { objects: Default::default(), state: Mutex::new(state) }
    }
}

/// An allocator for `DeviceType`
struct DeviceTypeAllocator {
    /// The available ranges of device types to allocate.
    ///
    /// Devices will be allocated from the back of the vector first.
    freelist: Vec<Range<DeviceType>>,
}

impl DeviceTypeAllocator {
    /// Create an allocator for the given ranges of device types.
    ///
    /// The devices will be allocated from the front of the vector first.
    fn new(mut available: Vec<Range<DeviceType>>) -> Self {
        available.reverse();
        Self { freelist: available }
    }

    /// Allocate a `DeviceType`.
    ///
    /// Once allocated, there is no mechanism for freeing a `DeviceType`.
    fn allocate(&mut self) -> Result<DeviceType, Errno> {
        let Some(range) = self.freelist.pop() else {
            return error!(ENOMEM);
        };
        let allocated = range.start;
        if let Some(next) = allocated.next_minor() {
            if next < range.end {
                self.freelist.push(next..range.end);
            }
        }
        Ok(allocated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::kobject::KObjectBased;
    use crate::device::mem::DevNull;
    use crate::fs::sysfs::DeviceDirectory;
    use crate::testing::*;
    use crate::vfs::*;
    use starnix_uapi::device_type::{INPUT_MAJOR, MEM_MAJOR};

    #[::fuchsia::test]
    fn registry_fails_to_add_duplicate_device() {
        let registry = DeviceRegistry::default();
        registry
            .register_major("mem".into(), DeviceMode::Char, MEM_MAJOR, simple_device_ops::<DevNull>)
            .expect("registers once");
        registry
            .register_major("random".into(), DeviceMode::Char, 123, simple_device_ops::<DevNull>)
            .expect("registers unique");
        registry
            .register_major("mem".into(), DeviceMode::Char, MEM_MAJOR, simple_device_ops::<DevNull>)
            .expect_err("fail to register duplicate");
    }

    #[::fuchsia::test]
    async fn registry_opens_device() {
        let (_kernel, current_task, mut locked) = create_kernel_task_and_unlocked();

        let registry = DeviceRegistry::default();
        registry
            .register_major("mem".into(), DeviceMode::Char, MEM_MAJOR, simple_device_ops::<DevNull>)
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
        let device = registry
            .register_dyn_device(
                &mut locked,
                &current_task,
                "test-device".into(),
                registry.objects.virtual_block_class(),
                DeviceDirectory::new,
                create_test_device,
            )
            .unwrap();
        let device_type = device.metadata.expect("has metadata").device_type;
        assert!(DYN_MAJOR_RANGE.contains(&device_type.major()));

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
        registry
            .register_major(
                "input".into(),
                DeviceMode::Char,
                INPUT_MAJOR,
                simple_device_ops::<DevNull>,
            )
            .expect("can register input");

        let input_class =
            registry.objects.get_or_create_class("input".into(), registry.objects.virtual_bus());
        registry.add_device(
            &mut locked,
            &current_task,
            "mice".into(),
            DeviceMetadata::new("mice".into(), DeviceType::new(INPUT_MAJOR, 0), DeviceMode::Char),
            input_class,
            DeviceDirectory::new,
        );

        assert!(registry.objects.class.has_child("input".into()));
        assert!(registry
            .objects
            .class
            .get_child("input".into())
            .and_then(|collection| collection.get_child("mice".into()))
            .is_some());
    }

    #[::fuchsia::test]
    async fn registry_add_bus() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let registry = &kernel.device_registry;
        registry
            .register_major(
                "input".into(),
                DeviceMode::Char,
                INPUT_MAJOR,
                simple_device_ops::<DevNull>,
            )
            .expect("can register input");

        let bus = registry.objects.get_or_create_bus("bus".into());
        let class = registry.objects.get_or_create_class("class".into(), bus);
        registry.add_device(
            &mut locked,
            &current_task,
            "device".into(),
            DeviceMetadata::new("device".into(), DeviceType::new(INPUT_MAJOR, 0), DeviceMode::Char),
            class,
            DeviceDirectory::new,
        );
        assert!(registry.objects.bus.has_child("bus".into()));
        assert!(registry
            .objects
            .bus
            .get_child("bus".into())
            .and_then(|collection| collection.get_child("device".into()))
            .is_some());
    }

    #[::fuchsia::test]
    async fn registry_remove_device() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let registry = &kernel.device_registry;
        registry
            .register_major(
                "input".into(),
                DeviceMode::Char,
                INPUT_MAJOR,
                simple_device_ops::<DevNull>,
            )
            .expect("can register input");

        let pci_bus = registry.objects.get_or_create_bus("pci".into());
        let input_class = registry.objects.get_or_create_class("input".into(), pci_bus);
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
            .objects
            .bus
            .get_child("pci".into())
            .expect("get pci collection")
            .has_child("mice".into()));
        assert!(!registry
            .objects
            .class
            .get_child("input".into())
            .expect("get input collection")
            .has_child("mice".into()));
    }

    #[::fuchsia::test]
    async fn registry_add_and_remove_numberless_device() {
        let (kernel, current_task, mut locked) = create_kernel_task_and_unlocked();
        let registry = &kernel.device_registry;

        let cooling_device = registry.add_numberless_device(
            &mut locked,
            &current_task,
            "cooling_device0".into(),
            registry.objects.virtual_thermal_class(),
            DeviceDirectory::new,
        );

        assert!(registry
            .objects
            .class
            .get_child("thermal".into())
            .expect("get thermal class")
            .has_child("cooling_device0".into()));

        registry.remove_device(&mut locked, &current_task, cooling_device);

        assert!(!registry
            .objects
            .class
            .get_child("thermal".into())
            .expect("get thermal class")
            .has_child("cooling_device0".into()));
    }
}
