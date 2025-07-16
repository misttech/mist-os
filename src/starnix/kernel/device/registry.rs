// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::kobject::{Class, Device, DeviceMetadata, UEventAction, UEventContext};
use crate::device::kobject_store::KObjectStore;
use crate::fs::devtmpfs::{devtmpfs_create_device, devtmpfs_remove_path};
use crate::fs::sysfs::build_device_directory;
use crate::task::{
    register_delayed_release, CurrentTask, CurrentTaskAndLocked, Kernel, KernelOrTask, SimpleWaiter,
};
use crate::vfs::pseudo::simple_directory::SimpleDirectoryMutator;
use crate::vfs::{FileOps, FsNode, FsStr, FsString};
use starnix_lifecycle::{ObjectReleaser, ReleaserAction};
use starnix_logging::log_error;
use starnix_sync::{InterruptibleEvent, LockEqualOrBefore, OrderedMutex};
use starnix_types::ownership::{Releasable, ReleaseGuard};
use starnix_uapi::as_any::AsAny;
use starnix_uapi::device_type::{
    DeviceType, DYN_MAJOR_RANGE, MISC_DYNANIC_MINOR_RANGE, MISC_MAJOR,
};
use starnix_uapi::error;
use starnix_uapi::errors::Errno;
use starnix_uapi::open_flags::OpenFlags;

use starnix_sync::{DeviceOpen, FileOpsCore, LockBefore, Locked, MappedMutexGuard, MutexGuard};
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

pub trait DeviceOps: DynClone + Send + Sync + AsAny + 'static {
    /// Instantiate a FileOps for this device.
    ///
    /// This function is called when userspace opens a file with a `DeviceType`
    /// assigned to this device.
    fn open(
        &self,
        _locked: &mut Locked<DeviceOpen>,
        _current_task: &CurrentTask,
        _device_type: DeviceType,
        _node: &FsNode,
        _flags: OpenFlags,
    ) -> Result<Box<dyn FileOps>, Errno>;

    fn unregister(self: Box<Self>, _locked: &mut Locked<FileOpsCore>, _current_task: &CurrentTask) {
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
            &mut Locked<DeviceOpen>,
            &CurrentTask,
            DeviceType,
            &FsNode,
            OpenFlags,
        ) -> Result<Box<dyn FileOps>, Errno>
        + 'static,
{
    fn open(
        &self,
        locked: &mut Locked<DeviceOpen>,
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
    _locked: &mut Locked<DeviceOpen>,
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

pub struct DeviceOpsWrapper(Box<dyn DeviceOps>);
impl ReleaserAction<DeviceOpsWrapper> for DeviceOpsWrapper {
    fn release(device_ops: ReleaseGuard<DeviceOpsWrapper>) {
        register_delayed_release(device_ops);
    }
}
impl Deref for DeviceOpsWrapper {
    type Target = dyn DeviceOps;
    fn deref(&self) -> &Self::Target {
        self.0.deref()
    }
}
pub type DeviceReleaser = ObjectReleaser<DeviceOpsWrapper, DeviceOpsWrapper>;
pub type DeviceHandle = Arc<DeviceReleaser>;
impl Releasable for DeviceOpsWrapper {
    type Context<'a> = CurrentTaskAndLocked<'a>;

    fn release<'a>(self, context: CurrentTaskAndLocked<'a>) {
        let (locked, current_task) = context;
        self.0.unregister(locked, current_task);
    }
}

/// An entry in the `DeviceRegistry`.
struct DeviceEntry {
    /// The name of the device.
    ///
    /// This name is the same as the name of the KObject for the device.
    name: FsString,

    /// The ops used to open the device.
    ops: DeviceHandle,
}

impl DeviceEntry {
    fn new(name: FsString, ops: impl DeviceOps) -> Self {
        Self { name, ops: Arc::new(DeviceOpsWrapper(Box::new(ops)).into()) }
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
    fn get(&self, device_type: DeviceType) -> Result<DeviceHandle, Errno> {
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
    state: OrderedMutex<DeviceRegistryState, starnix_sync::DeviceRegistryState>,
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
        locked: &mut Locked<L>,
        kernel: &Kernel,
        device: Device,
        event: Option<Arc<InterruptibleEvent>>,
    ) where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        if let Some(metadata) = &device.metadata {
            devtmpfs_create_device(kernel, metadata.clone(), event);
            self.dispatch_uevent(locked, UEventAction::Add, device);
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
    pub fn register_device<'a, L>(
        &self,
        locked: &mut Locked<L>,
        kernel_or_task: impl KernelOrTask<'a>,
        name: &FsStr,
        metadata: DeviceMetadata,
        class: Class,
        dev_ops: impl DeviceOps,
    ) -> Result<Device, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.register_device_with_dir(
            locked,
            kernel_or_task,
            name,
            metadata,
            class,
            build_device_directory,
            dev_ops,
        )
    }

    /// Register a device with a custom directory.
    ///
    /// See `register_device` for an explanation of the parameters.
    pub fn register_device_with_dir<'a, L>(
        &self,
        locked: &mut Locked<L>,
        kernel_or_task: impl KernelOrTask<'a>,
        name: &FsStr,
        metadata: DeviceMetadata,
        class: Class,
        build_directory: impl FnOnce(&Device, &SimpleDirectoryMutator),
        dev_ops: impl DeviceOps,
    ) -> Result<Device, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let entry = DeviceEntry::new(name.into(), dev_ops);
        self.devices(locked, metadata.mode).register_minor(metadata.device_type, entry);
        self.add_device(locked, kernel_or_task, name, metadata, class, build_directory)
    }

    /// Register a dynamic device in the `MISC_MAJOR` major device number.
    ///
    /// MISC devices (major number 10) with minor numbers in the range 52..128 are dynamically
    /// assigned. Rather than hardcoding registrations with these device numbers, use this
    /// function instead to register the device.
    ///
    /// See `register_device` for an explanation of the parameters.
    pub fn register_misc_device<'a, L>(
        &self,
        locked: &mut Locked<L>,
        kernel_or_task: impl KernelOrTask<'a>,
        name: &FsStr,
        dev_ops: impl DeviceOps,
    ) -> Result<Device, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let device_type =
            self.state.lock(locked.cast_locked()).misc_chardev_allocator.allocate()?;
        let metadata = DeviceMetadata::new(name.into(), device_type, DeviceMode::Char);
        Ok(self.register_device(
            locked,
            kernel_or_task,
            name,
            metadata,
            self.objects.misc_class(),
            dev_ops,
        )?)
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
    pub fn register_dyn_device<'a, L>(
        &self,
        locked: &mut Locked<L>,
        kernel_or_task: impl KernelOrTask<'a>,
        name: &FsStr,
        class: Class,
        dev_ops: impl DeviceOps,
    ) -> Result<Device, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.register_dyn_device_with_dir(
            locked,
            kernel_or_task,
            name,
            class,
            build_device_directory,
            dev_ops,
        )
    }

    /// Register a dynamic device with a custom directory.
    ///
    /// See `register_device` for an explanation of the parameters.
    pub fn register_dyn_device_with_dir<'a, L>(
        &self,
        locked: &mut Locked<L>,
        kernel_or_task: impl KernelOrTask<'a>,
        name: &FsStr,
        class: Class,
        build_directory: impl FnOnce(&Device, &SimpleDirectoryMutator),
        dev_ops: impl DeviceOps,
    ) -> Result<Device, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.register_dyn_device_with_devname(
            locked,
            kernel_or_task,
            name,
            name,
            class,
            build_directory,
            dev_ops,
        )
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
    pub fn register_dyn_device_with_devname<'a, L>(
        &self,
        locked: &mut Locked<L>,
        kernel_or_task: impl KernelOrTask<'a>,
        name: &FsStr,
        devname: &FsStr,
        class: Class,
        build_directory: impl FnOnce(&Device, &SimpleDirectoryMutator),
        dev_ops: impl DeviceOps,
    ) -> Result<Device, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let device_type = self.state.lock(locked.cast_locked()).dyn_chardev_allocator.allocate()?;
        let metadata = DeviceMetadata::new(devname.into(), device_type, DeviceMode::Char);
        Ok(self.register_device_with_dir(
            locked,
            kernel_or_task,
            name,
            metadata,
            class,
            build_directory,
            dev_ops,
        )?)
    }

    /// Register a "silent" dynamic device with major numbers 234..255.
    ///
    /// Only use for devices that should not be registered with the KObjectStore/appear in sysfs.
    /// This is a rare occurrence.
    ///
    /// See `register_dyn_device` for an explanation of dyn devices and of the parameters.
    pub fn register_silent_dyn_device<'a, L>(
        &self,
        locked: &mut Locked<L>,
        name: &FsStr,
        dev_ops: impl DeviceOps,
    ) -> Result<DeviceMetadata, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let device_type = self.state.lock(locked.cast_locked()).dyn_chardev_allocator.allocate()?;
        let metadata = DeviceMetadata::new(name.into(), device_type, DeviceMode::Char);
        let entry = DeviceEntry::new(name.into(), dev_ops);
        self.devices(locked, metadata.mode).register_minor(metadata.device_type, entry);
        Ok(metadata)
    }

    /// Directly add a device to the KObjectStore.
    ///
    /// This function should be used only by device that have registered an entire major device
    /// number. If you want to add a single minor device, use the `register_device` function
    /// instead.
    ///
    /// See `register_device` for an explanation of the parameters.
    pub fn add_device<'a, L>(
        &self,
        locked: &mut Locked<L>,
        kernel_or_task: impl KernelOrTask<'a>,
        name: &FsStr,
        metadata: DeviceMetadata,
        class: Class,
        build_directory: impl FnOnce(&Device, &SimpleDirectoryMutator),
    ) -> Result<Device, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.devices(locked, metadata.mode)
            .get(metadata.device_type)
            .expect("device is registered");
        let device = self.objects.create_device(name, Some(metadata), class, build_directory);

        block_task_until(kernel_or_task, |kernel, event| {
            Ok(self.notify_device(locked, kernel, device.clone(), event))
        })?;
        Ok(device)
    }

    /// Add a net device to the device registry.
    ///
    /// Net devices are different from other devices because they do not have a device number.
    /// Instead, their `uevent` files have the following format:
    ///
    /// ```
    /// INTERFACE={name}
    /// IFINDEX={index}
    /// ```
    ///
    /// Currently, we only register the net devices by name and use an empty `uevent` file.
    pub fn add_net_device(&self, name: &FsStr) -> Device {
        self.objects.create_device(name, None, self.objects.net_class(), build_device_directory)
    }

    /// Remove a net device from the device registry.
    ///
    /// See `add_net_device` for more details.
    pub fn remove_net_device(&self, device: Device) {
        assert!(device.metadata.is_none());
        self.objects.remove(&device);
    }

    /// Directly add a device to the KObjectStore that lacks a device number.
    ///
    /// This function should be used only by device do not have a major or a minor number. You can
    /// identify these devices because they appear in sysfs and have an empty `uevent` file.
    ///
    /// See `register_device` for an explanation of the parameters.
    pub fn add_numberless_device<L>(
        &self,
        _locked: &mut Locked<L>,
        name: &FsStr,
        class: Class,
        build_directory: impl FnOnce(&Device, &SimpleDirectoryMutator),
    ) -> Device
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.objects.create_device(name, None, class, build_directory)
    }

    /// Remove a device directly added with `add_device`.
    ///
    /// This function should be used only by device that have registered an entire major device
    /// number. Individually registered minor device cannot be removed at this time.
    pub fn remove_device<L>(
        &self,
        locked: &mut Locked<L>,
        current_task: &CurrentTask,
        device: Device,
    ) where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        if let Some(metadata) = &device.metadata {
            self.dispatch_uevent(locked, UEventAction::Remove, device.clone());

            if let Err(err) = devtmpfs_remove_path(locked, current_task, metadata.devname.as_ref())
            {
                log_error!("Cannot remove device {:?} ({:?})", device, err);
            }
        }

        self.objects.remove(&device);
    }

    /// Returns a list of the registered major device numbers for the given `DeviceMode` and their
    /// names.
    pub fn list_major_devices<L>(
        &self,
        locked: &mut Locked<L>,
        mode: DeviceMode,
    ) -> Vec<(u32, FsString)>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.devices(locked, mode).list_major_devices()
    }

    /// Returns a list of the registered minor devices for the given `DeviceMode` and their names.
    pub fn list_minor_devices<L>(
        &self,
        locked: &mut Locked<L>,
        mode: DeviceMode,
        range: Range<DeviceType>,
    ) -> Vec<(DeviceType, FsString)>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.devices(locked, mode).list_minor_devices(range)
    }

    /// The `RegisteredDevice` object for the given `DeviceMode`.
    fn devices<'a, L>(
        &'a self,
        locked: &'a mut Locked<L>,
        mode: DeviceMode,
    ) -> MappedMutexGuard<'a, RegisteredDevices>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        MutexGuard::map(self.state.lock(locked.cast_locked()), |state| match mode {
            DeviceMode::Char => &mut state.char_devices,
            DeviceMode::Block => &mut state.block_devices,
        })
    }

    /// Register an entire major device number.
    ///
    /// If you register an entire major device, use `add_device` and `remove_device` to manage the
    /// sysfs entiries for your device rather than trying to register and unregister individual
    /// minor devices.
    pub fn register_major<'a, L>(
        &self,
        locked: &mut Locked<L>,
        name: FsString,
        mode: DeviceMode,
        major: u32,
        dev_ops: impl DeviceOps,
    ) -> Result<(), Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let entry = DeviceEntry::new(name, dev_ops);
        self.devices(locked, mode).register_major(major, entry)
    }

    /// Allocate an anonymous device identifier.
    pub fn next_anonymous_dev_id<'a, L>(&self, locked: &mut Locked<L>) -> DeviceType
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let mut state = self.state.lock(locked.cast_locked());
        let id = DeviceType::new(0, state.next_anon_minor);
        state.next_anon_minor += 1;
        id
    }

    /// Register a new listener for uevents on devices.
    ///
    /// Returns a key used to unregister the listener.
    pub fn register_listener<'a, L>(
        &self,
        locked: &mut Locked<L>,
        listener: impl DeviceListener + 'static,
    ) -> DeviceListenerKey
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let mut state = self.state.lock(locked.cast_locked());
        let key = state.next_listener_id;
        state.next_listener_id += 1;
        state.listeners.insert(key, Box::new(listener));
        key
    }

    /// Unregister a listener previously registered through `register_listener`.
    pub fn unregister_listener<'a, L>(&self, locked: &mut Locked<L>, key: &DeviceListenerKey)
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.state.lock(locked.cast_locked()).listeners.remove(key);
    }

    /// Dispatch an uevent for the given `device`.
    pub fn dispatch_uevent<'a, L>(
        &self,
        locked: &mut Locked<L>,
        action: UEventAction,
        device: Device,
    ) where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        let mut state = self.state.lock(locked.cast_locked());
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
        locked: &mut Locked<L>,
        current_task: &CurrentTask,
        node: &FsNode,
        flags: OpenFlags,
        device_type: DeviceType,
        mode: DeviceMode,
    ) -> Result<Box<dyn FileOps>, Errno>
    where
        L: LockBefore<DeviceOpen>,
    {
        let locked = locked.cast_locked::<DeviceOpen>();
        let dev_ops = self.devices(locked, mode).get(device_type)?;
        dev_ops.open(locked, current_task, device_type, node, flags)
    }

    pub fn get_device<L>(
        &self,
        locked: &mut Locked<L>,
        device_type: DeviceType,
        mode: DeviceMode,
    ) -> Result<DeviceHandle, Errno>
    where
        L: LockEqualOrBefore<FileOpsCore>,
    {
        self.devices(locked, mode).get(device_type)
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
        Self { objects: Default::default(), state: OrderedMutex::new(state) }
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

/// Run the given closure and blocks the thread until the passed event is signaled, if this is
/// built from a `CurrentTask`, otherwise does not block.
fn block_task_until<'a, T, F>(kernel_or_task: impl KernelOrTask<'a>, f: F) -> Result<T, Errno>
where
    F: FnOnce(&Kernel, Option<Arc<InterruptibleEvent>>) -> Result<T, Errno>,
{
    let kernel = kernel_or_task.kernel();
    match kernel_or_task.maybe_task() {
        None => f(kernel, None),
        Some(task) => {
            let event = InterruptibleEvent::new();
            let (_waiter, guard) = SimpleWaiter::new(&event);
            let result = f(kernel, Some(event.clone()))?;
            task.block_until(guard, zx::MonotonicInstant::INFINITE)?;
            Ok(result)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::mem::DevNull;
    use crate::testing::*;
    use crate::vfs::*;
    use starnix_uapi::device_type::{INPUT_MAJOR, MEM_MAJOR};

    #[::fuchsia::test]
    async fn registry_fails_to_add_duplicate_device() {
        let (_kernel, _current_task, locked) = create_kernel_task_and_unlocked();

        let registry = DeviceRegistry::default();
        registry
            .register_major(
                locked,
                "mem".into(),
                DeviceMode::Char,
                MEM_MAJOR,
                simple_device_ops::<DevNull>,
            )
            .expect("registers once");
        registry
            .register_major(
                locked,
                "random".into(),
                DeviceMode::Char,
                123,
                simple_device_ops::<DevNull>,
            )
            .expect("registers unique");
        registry
            .register_major(
                locked,
                "mem".into(),
                DeviceMode::Char,
                MEM_MAJOR,
                simple_device_ops::<DevNull>,
            )
            .expect_err("fail to register duplicate");
    }

    #[::fuchsia::test]
    async fn registry_opens_device() {
        let (kernel, current_task, locked) = create_kernel_task_and_unlocked();

        let registry = DeviceRegistry::default();
        registry
            .register_major(
                locked,
                "mem".into(),
                DeviceMode::Char,
                MEM_MAJOR,
                simple_device_ops::<DevNull>,
            )
            .expect("registers unique");

        let fs = create_testfs(locked, &kernel);
        let node = create_fs_node_for_testing(&fs, PanickingFsNode);

        // Fail to open non-existent device.
        assert!(registry
            .open_device(
                locked,
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
                locked,
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
                locked,
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
        let (kernel, current_task, locked) = create_kernel_task_and_unlocked();

        fn create_test_device(
            _locked: &mut Locked<DeviceOpen>,
            _current_task: &CurrentTask,
            _id: DeviceType,
            _node: &FsNode,
            _flags: OpenFlags,
        ) -> Result<Box<dyn FileOps>, Errno> {
            Ok(Box::new(PanickingFile))
        }

        let registry = &kernel.device_registry;
        let device = registry
            .register_dyn_device(
                locked,
                &current_task,
                "test-device".into(),
                registry.objects.virtual_block_class(),
                create_test_device,
            )
            .unwrap();
        let device_type = device.metadata.expect("has metadata").device_type;
        assert!(DYN_MAJOR_RANGE.contains(&device_type.major()));

        let fs = create_testfs(locked, &kernel);
        let node = create_fs_node_for_testing(&fs, PanickingFsNode);
        let _ = registry
            .open_device(
                locked,
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
        let (kernel, current_task, locked) = create_kernel_task_and_unlocked();
        let registry = &kernel.device_registry;
        registry
            .register_major(
                locked,
                "input".into(),
                DeviceMode::Char,
                INPUT_MAJOR,
                simple_device_ops::<DevNull>,
            )
            .expect("can register input");

        let input_class =
            registry.objects.get_or_create_class("input".into(), registry.objects.virtual_bus());
        registry
            .add_device(
                locked,
                &current_task,
                "mouse".into(),
                DeviceMetadata::new(
                    "mouse".into(),
                    DeviceType::new(INPUT_MAJOR, 0),
                    DeviceMode::Char,
                ),
                input_class,
                build_device_directory,
            )
            .expect("add_device");

        assert!(registry.objects.root.lookup("class/input/mouse".into()).is_some());
    }

    #[::fuchsia::test]
    async fn registry_add_bus() {
        let (kernel, current_task, locked) = create_kernel_task_and_unlocked();
        let registry = &kernel.device_registry;
        registry
            .register_major(
                locked,
                "input".into(),
                DeviceMode::Char,
                INPUT_MAJOR,
                simple_device_ops::<DevNull>,
            )
            .expect("can register input");

        let bus = registry.objects.get_or_create_bus("my-bus".into());
        let class = registry.objects.get_or_create_class("my-class".into(), bus);
        registry
            .add_device(
                locked,
                &current_task,
                "my-device".into(),
                DeviceMetadata::new(
                    "my-device".into(),
                    DeviceType::new(INPUT_MAJOR, 0),
                    DeviceMode::Char,
                ),
                class,
                build_device_directory,
            )
            .expect("add_device");
        assert!(registry.objects.root.lookup("bus/my-bus".into()).is_some());
        assert!(registry.objects.root.lookup("devices/my-bus/my-class".into()).is_some());
        assert!(registry.objects.root.lookup("devices/my-bus/my-class/my-device".into()).is_some());
    }

    #[::fuchsia::test]
    async fn registry_remove_device() {
        let (kernel, current_task, locked) = create_kernel_task_and_unlocked();
        let registry = &kernel.device_registry;
        registry
            .register_major(
                locked,
                "input".into(),
                DeviceMode::Char,
                INPUT_MAJOR,
                simple_device_ops::<DevNull>,
            )
            .expect("can register input");

        let pci_bus = registry.objects.get_or_create_bus("pci".into());
        let input_class = registry.objects.get_or_create_class("input".into(), pci_bus);
        let mouse_dev = registry
            .add_device(
                locked,
                &current_task,
                "mouse".into(),
                DeviceMetadata::new(
                    "mouse".into(),
                    DeviceType::new(INPUT_MAJOR, 0),
                    DeviceMode::Char,
                ),
                input_class.clone(),
                build_device_directory,
            )
            .expect("add_device");

        assert!(registry.objects.root.lookup("bus/pci/devices/mouse".into()).is_some());
        assert!(registry.objects.root.lookup("devices/pci/input/mouse".into()).is_some());
        assert!(registry.objects.root.lookup("class/input/mouse".into()).is_some());

        registry.remove_device(locked, &current_task, mouse_dev);

        assert!(registry.objects.root.lookup("bus/pci/devices/mouse".into()).is_none());
        assert!(registry.objects.root.lookup("devices/pci/input/mouse".into()).is_none());
        assert!(registry.objects.root.lookup("class/input/mouse".into()).is_none());
    }

    #[::fuchsia::test]
    async fn registry_add_and_remove_numberless_device() {
        let (kernel, current_task, locked) = create_kernel_task_and_unlocked();
        let registry = &kernel.device_registry;

        let cooling_device = registry.add_numberless_device(
            locked,
            "cooling_device0".into(),
            registry.objects.virtual_thermal_class(),
            build_device_directory,
        );

        assert!(registry.objects.root.lookup("class/thermal/cooling_device0".into()).is_some());
        assert!(registry
            .objects
            .root
            .lookup("devices/virtual/thermal/cooling_device0".into())
            .is_some());

        registry.remove_device(locked, &current_task, cooling_device);

        assert!(registry.objects.root.lookup("class/thermal/cooling_device0".into()).is_none());
        assert!(registry
            .objects
            .root
            .lookup("devices/virtual/thermal/cooling_device0".into())
            .is_none());
    }
}
