// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_DEVICE_REGISTRY_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_DEVICE_REGISTRY_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/device/device_mode.h>
#include <lib/mistos/starnix/kernel/device/kobject_store.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/device_type.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/util/btree_map.h>
#include <lib/mistos/util/default_construct.h>
#include <lib/mistos/util/range-map.h>
#include <lib/starnix_sync/locks.h>

#include <algorithm>

#include <ktl/unique_ptr.h>

namespace starnix {

class CurrentTask;
class DeviceOpen;
class FileOps;
class FsNode;

using starnix_uapi::OpenFlags;

/// Interface for device operations.
///
/// This interface is used to instantiate file operations when userspace opens a file
/// with a DeviceType assigned to this device.
class DeviceOps : public fbl::RefCounted<DeviceOps> {
 public:
  virtual ~DeviceOps() = default;

  /// Instantiate a FileOps for this device.
  ///
  /// This function is called when userspace opens a file with a DeviceType
  /// assigned to this device.
  virtual fit::result<Errno, ktl::unique_ptr<FileOps>> open(const CurrentTask& current_task,
                                                            DeviceType device_type,
                                                            const FsNode& node,
                                                            OpenFlags flags) = 0;
};

/// Allows directly using a function or closure as an implementation of DeviceOps, avoiding having
/// to write a zero-size struct and an impl for it.
template <typename F>
class FunctionDeviceOps : public DeviceOps {
 public:
  explicit FunctionDeviceOps(F func) : open_impl_(ktl::move(func)) {}

  fit::result<Errno, ktl::unique_ptr<FileOps>> open(const CurrentTask& current_task,
                                                    DeviceType device_type, const FsNode& node,
                                                    OpenFlags flags) final {
    return open_impl_(current_task, device_type, node, flags);
  }

 private:
  F open_impl_;
};

template <typename T>
  requires std::is_base_of_v<FileOps, T> && std::is_default_constructible_v<T>
fit::result<Errno, ktl::unique_ptr<FileOps>> open_impl(const CurrentTask& current_task,
                                                       DeviceType device_type, const FsNode& node,
                                                       OpenFlags flags) {
  fbl::AllocChecker ac;
  auto ptr = new (&ac) T();
  if (!ac.check()) {
    return fit::error(errno(ENOMEM));
  }
  return fit::ok(ktl::unique_ptr<FileOps>(ptr));
}

/// A simple `DeviceOps` function for any device that implements `FileOps + Default`.
template <typename T>
  requires std::is_base_of_v<FileOps, T> && std::is_default_constructible_v<T>
DeviceOps* simple_device_ops() {
  fbl::AllocChecker ac;
  auto ptr = new (&ac) FunctionDeviceOps(std::move(open_impl<T>));
  if (!ac.check()) {
    return nullptr;
  }
  return ptr;
}

/// An entry in the `DeviceRegistry`.
class DeviceEntry {
 private:
  /// The name of the device.
  ///
  /// This name is the same as the name of the KObject for the device.
  FsString name_;

  /// The ops used to open the device.
  fbl::RefPtr<DeviceOps> ops_;

  // impl DeviceEntry
 public:
  static DeviceEntry New(FsString name, DeviceOps* ops) {
    return DeviceEntry(ktl::move(name), ops);
  }

  // C++
  DeviceEntry(FsString name, DeviceOps* ops) : name_(ktl::move(name)), ops_(fbl::AdoptRef(ops)) {}

  DeviceEntry(DeviceEntry&& other) : name_(ktl::move(other.name_)), ops_(ktl::move(other.ops_)) {}
  DeviceEntry& operator=(DeviceEntry&& other) {
    name_ = ktl::move(other.name_);
    ops_ = ktl::move(other.ops_);
    return *this;
  }

  DeviceEntry(const DeviceEntry& other) = default;
  DeviceEntry& operator=(const DeviceEntry& other) {
    if (this != &other) {
      name_ = other.name_;
      ops_ = other.ops_;
    }
    return *this;
  }

  bool operator==(const DeviceEntry& other) const {
    return name_ == other.name_ && ops_ == other.ops_;
  }

 private:
  // DISALLOW_COPY_AND_ASSIGN_ALLOW_MOVE(DeviceEntry);
  friend class RegisteredDevices;
};

/// The devices registered for a given `DeviceMode`.
///
/// Each `DeviceMode` has its own namespace of registered devices.
class RegisteredDevices {
 private:
  /// The major devices registered for this device mode.
  ///
  /// Typically the devices registered here will add and remove individual devices using the
  /// `add_device` and `remove_device` functions on `DeviceRegistry`.
  ///
  /// A major device registration shadows any minor device registrations for the same major
  /// device number. We might need to reconsider this choice in the future in order to make
  /// the /proc/devices file correctly list major devices such as `misc`.
  // util::BTreeMap<uint32_t, DeviceEntry> majors_;
  util::BTreeMap<uint32_t, DeviceEntry> majors_;

  /// Individually registered minor devices.
  ///
  /// These devices are registered using the `register_device` function on `DeviceRegistry`.
  util::BTreeMap<DeviceType, DeviceEntry> minors_;

  // impl RegisteredDevices
  /// Register a major device.
  ///
  /// Returns `EINVAL` if the major device is already registered.
  fit::result<Errno> register_major(uint32_t major, DeviceEntry entry) {
    if (auto slot = majors_.try_emplace(major, ktl::move(entry)); slot.second) {
      return fit::ok();
    }
    return fit::error(errno(EINVAL));
  }

  /// Register a minor device.
  ///
  /// Overwrites any existing minor device registered with the given `DeviceType`.
  void register_minor(starnix_uapi::DeviceType device_type, DeviceEntry entry) {
    minors_.insert_or_assign(device_type, ktl::move(entry));
  }

  /// Get the ops for a given `DeviceType`.
  ///
  /// If there is a major device registered with the major device number of the
  /// `DeviceType`, the ops for that major device will be returned. Otherwise,
  /// if there is a minor device registered, the ops for that minor device will be
  /// returned. Otherwise, returns `ENODEV`.
  fit::result<Errno, fbl::RefPtr<DeviceOps>> get(starnix_uapi::DeviceType device_type) const {
    if (auto major_device = majors_.find(device_type.major()); major_device != majors_.end()) {
      return fit::ok(major_device->second.ops_);
    }
    if (auto minor_device = minors_.find(device_type); minor_device != minors_.end()) {
      return fit::ok(minor_device->second.ops_);
    }
    return fit::error(errno(ENODEV));
  }

  /// Returns a list of the registered major device numbers and their names.
  fbl::Vector<ktl::pair<uint32_t, FsString>> list_major_devices() const {
    fbl::AllocChecker ac;
    fbl::Vector<ktl::pair<uint32_t, FsString>> result;
    for (const auto& [major, entry] : majors_) {
      result.push_back({major, entry.name_}, &ac);
      ZX_ASSERT(ac.check());
    }
    return result;
  }

  /// Returns a list of the registered minor devices and their names.
  fbl::Vector<ktl::pair<starnix_uapi::DeviceType, FsString>> list_minor_devices(
      util::Range<starnix_uapi::DeviceType> range) const {
    fbl::AllocChecker ac;
    fbl::Vector<ktl::pair<starnix_uapi::DeviceType, FsString>> result;
    for (auto it = minors_.lower_bound(range.start); it != minors_.end() && it->first < range.end;
         ++it) {
      result.push_back({it->first, it->second.name_}, &ac);
      ZX_ASSERT(ac.check());
    }
    return result;
  }

  // C++
 public:
  friend class DeviceRegistry;
  RegisteredDevices() = default;
};

/// An allocator for `DeviceType`
class DeviceTypeAllocator {
 private:
  /// The available ranges of device types to allocate.
  ///
  /// Devices will be allocated from the back of the vector first.
  fbl::Vector<util::Range<starnix_uapi::DeviceType>> freelist_;

  // impl DeviceTypeAllocator

 public:
  /// Create an allocator for the given ranges of device types.
  ///
  /// The devices will be allocated from the front of the vector first.
  static DeviceTypeAllocator New(fbl::Vector<util::Range<starnix_uapi::DeviceType>> available) {
    // Reverse the vector since we'll allocate from back
    for (size_t i = 0; i < available.size() / 2; i++) {
      auto temp = available[i];
      available[i] = available[available.size() - 1 - i];
      available[available.size() - 1 - i] = temp;
    }
    return DeviceTypeAllocator(ktl::move(available));
  }

  /// Allocate a `DeviceType`.
  ///
  /// Once allocated, there is no mechanism for freeing a `DeviceType`.
  fit::result<Errno, starnix_uapi::DeviceType> allocate() {
    if (freelist_.is_empty()) {
      return fit::error(errno(ENOMEM));
    }
    auto range = freelist_.erase(freelist_.size() - 1);

    auto allocated = range.start;
    auto next = allocated.next_minor();
    if (next < range.end) {
      fbl::AllocChecker ac;
      freelist_.push_back(util::Range<starnix_uapi::DeviceType>{.start = next, .end = range.end},
                          &ac);
      ZX_ASSERT(ac.check());
    }

    return fit::ok(allocated);
  }

  // C++
  DeviceTypeAllocator() = default;

 private:
  explicit DeviceTypeAllocator(fbl::Vector<util::Range<starnix_uapi::DeviceType>> available)
      : freelist_(ktl::move(available)) {}
};

/// State for the device registry.
struct DeviceRegistryState {
  /// The registered character devices.
  RegisteredDevices char_devices;

  /// The registered block devices.
  RegisteredDevices block_devices;

  /// Some of the misc devices (devices with the `MISC_MAJOR` major number) are dynamically
  /// allocated. This allocator keeps track of which device numbers have been allocated to
  /// such devices.
  DeviceTypeAllocator misc_chardev_allocator;

  /// A range of large major device numbers are reserved for other dynamically allocated
  /// devices. This allocator keeps track of which device numbers have been allocated to
  /// such devices.
  DeviceTypeAllocator dyn_chardev_allocator;

  /// The next anonymous device number to assign to a file system.
  uint32_t next_anon_minor;

  /// Listeners registered to learn about new devices being added to the registry.
  ///
  /// These listeners generate uevents for those devices, which populates /dev on some
  /// systems.
  // std::map<uint64_t, std::unique_ptr<DeviceListener>> listeners;

  /// The next identifier to use for a listener.
  uint64_t next_listener_id;

  /// The next event identifier to use when notifying listeners.
  uint64_t next_event_id;
};

/// The registry for devices.
///
/// Devices are specified in file systems with major and minor device numbers, together referred to
/// as a `DeviceType`. When userspace opens one of those files, we look up the `DeviceType` in the
/// device registry to instantiate a file for that device.
///
/// The `DeviceRegistry` also manages the `KObjectStore`, which provides metadata for devices via
/// the sysfs file system, typically mounted at /sys.
class DeviceRegistry {
 public:
  /// The KObjects for registered devices.
  KObjectStore objects_;

 private:
  /// Mutable state for the device registry.
  mutable starnix_sync::Mutex<DeviceRegistryState> state_;

 public:
  // impl DeviceRegistry

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
  template <typename N, typename F>
    requires std::is_convertible_v<std::invoke_result_t<F, Device>, N*> &&
             std::is_base_of_v<FsNodeOps, N>
  Device register_device(const CurrentTask& current_task, const FsStr& name,
                         DeviceMetadata metadata, Class clazz, F&& create_device_sysfs_ops,
                         DeviceOps* dev_ops) {
    auto entry = DeviceEntry::New(name, dev_ops);
    devices(metadata.mode_)->register_minor(metadata.device_type_, ktl::move(entry));
    auto device = objects_.create_device<N>(name, metadata, clazz, create_device_sysfs_ops);
    // notify_device(current_task, device);
    return ktl::move(device);
  }

  /// The `RegisteredDevice` object for the given `DeviceMode`.
  starnix_sync::MappedMutexGuard<RegisteredDevices> devices(DeviceMode mode) const {
    return ktl::move(starnix_sync::MutexGuard<DeviceRegistryState>::map<RegisteredDevices>(
        state_.Lock(), [&mode](DeviceRegistryState* state) -> RegisteredDevices* {
          switch (mode.type_) {
            case DeviceMode::Type::kChar:
              return &state->char_devices;
            case DeviceMode::Type::kBlock:
              return &state->block_devices;
          }
        }));
  }

  /// Directly add a device to the KObjectStore.
  ///
  /// This function should be used only by device that have registered an entire major device
  /// number. If you want to add a single minor device, use the `register_device` function
  /// instead.
  ///
  /// See `register_device` for an explanation of the parameters.
  template <typename N, typename F>
    requires std::is_convertible_v<std::invoke_result_t<F, Device>, N*> &&
             std::is_base_of_v<FsNodeOps, N>
  Device add_device(const CurrentTask& current_task, const FsStr& name, DeviceMetadata metadata,
                    Class clazz, F&& create_device_sysfs_ops) {
    auto ops = devices(metadata.mode_)->get(metadata.device_type_);
    ZX_ASSERT_MSG(ops.is_ok(), "device is registered");

    auto device = objects_.create_device<N>(name, metadata, clazz, create_device_sysfs_ops);
    // notify_device(current_task, device);
    return ktl::move(device);
  }

  /// Remove a device directly added with `add_device`.
  ///
  /// This function should be used only by device that have registered an entire major device
  /// number. Individually registered minor device cannot be removed at this time.
  void remove_device(const CurrentTask& current_task, Device device) const {
    // dispatch_uevent(UEventAction::Remove, device);
    objects_.destroy_device(device);

    // TODO: Port devtmpfs_remove_node functionality
    // if (auto err = devtmpfs_remove_node(current_task, device.metadata().devname())) {
    //   log_error!("Cannot remove device {:?} ({:?})", device, err);
    // }
  }

  /// Register an entire major device number.
  ///
  /// If you register an entire major device, use `add_device` and `remove_device` to manage the
  /// sysfs entiries for your device rather than trying to register and unregister individual
  /// minor devices.
  fit::result<Errno> register_major(FsString name, DeviceMode mode, uint32_t major,
                                    DeviceOps* dev_ops) {
    DeviceEntry entry = DeviceEntry::New(ktl::move(name), dev_ops);
    return devices(mode)->register_major(major, ktl::move(entry));
  }

  /// Allocate an anonymous device identifier.
  DeviceType next_anonymous_dev_id() {
    auto state = state_.Lock();
    auto id = DeviceType::New(0, state->next_anon_minor);
    state->next_anon_minor++;
    return id;
  }

  /// Instantiate a file for the specified device.
  ///
  /// The device will be looked up in the device registry by `DeviceMode` and `DeviceType`.
  fit::result<Errno, ktl::unique_ptr<FileOps>> open_device(const CurrentTask& current_task,
                                                           const FsNode& node, OpenFlags flags,
                                                           DeviceType device_type,
                                                           DeviceMode mode) const {
    auto dev_ops = devices(mode)->get(device_type) _EP(dev_ops);
    return dev_ops->open(current_task, device_type, node, flags);
  }

 public:
  // impl Default for DeviceRegistry
  static DeviceRegistry Default() {
    fbl::AllocChecker ac;
    fbl::Vector<util::Range<DeviceType>> misc_available;

    misc_available.push_back(DeviceType::new_range(MISC_MAJOR, MISC_DYNANIC_MINOR_RANGE), &ac);
    ZX_ASSERT(ac.check());

    fbl::Vector<util::Range<DeviceType>> dyn_available;
    for (uint32_t major = DYN_MAJOR_RANGE.start; major < DYN_MAJOR_RANGE.end; major++) {
      dyn_available.push_back(
          DeviceType::new_range(major, DeviceMode(DeviceMode::Type::kChar).minor_range()), &ac);
      ZX_ASSERT(ac.check());
    }

    std::ranges::reverse(dyn_available);

    DeviceRegistryState state = {
        .char_devices = RegisteredDevices(),
        .block_devices = RegisteredDevices(),
        .misc_chardev_allocator = DeviceTypeAllocator::New(ktl::move(misc_available)),
        .dyn_chardev_allocator = DeviceTypeAllocator::New(ktl::move(dyn_available)),
        .next_anon_minor = 1,
        .next_listener_id = 0,
        .next_event_id = 0};

    return DeviceRegistry(KObjectStore::Default(), ktl::move(state));
  }

 private:
  explicit DeviceRegistry(KObjectStore objects, DeviceRegistryState state)
      : objects_(ktl::move(objects)) {
    *state_.Lock() = ktl::move(state);
  }
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_DEVICE_REGISTRY_H_
