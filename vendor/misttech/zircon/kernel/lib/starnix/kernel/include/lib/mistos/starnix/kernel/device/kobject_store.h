// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_DEVICE_KOBJECT_STORE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_DEVICE_KOBJECT_STORE_H_

#include <lib/mistos/starnix/kernel/device/kobject.h>
#include <lib/mistos/starnix/kernel/fs/sysfs/bus_collection_directory.h>
#include <lib/mistos/starnix/kernel/fs/sysfs/fs.h>
#include <lib/mistos/starnix/kernel/fs/sysfs/kobject_directory.h>
#include <lib/mistos/starnix/kernel/fs/sysfs/kobject_symlink_directory.h>

namespace starnix {

/// The owner of all the KObjects in sysfs.
///
/// This structure holds strong references to the KObjects that are visible in sysfs. These
/// objects are organized into hierarchies that make it easier to implement sysfs.

class KObjectStore {
 public:
  /// All of the devices added to the system.
  ///
  /// Used to populate the /sys/devices directory.
  KObjectHandle devices_;

  /// All of the device classes known to the system.
  ///
  /// Used to populate the /sys/class directory.
  KObjectHandle class_;

  /// All of the block devices known to the system.
  ///
  /// Used to populate the /sys/block directory.
  KObjectHandle block_;

  /// All of the buses known to the system.
  ///
  /// Devices are organized first by bus and then by class. The more relevant bus for our
  /// purposes is the "virtual" bus, which is accessible via the `virtual_bus` method.
  KObjectHandle bus_;

  /// The devices in the system, organized by DeviceMode and DeviceType.
  ///
  /// Used to populate the /sys/dev directory. The KObjects descended from this KObject are
  /// the same objects referenced through the `devices` KObject. They are just organized in
  /// a different hierarchy. When populated in /sys/dev, they appear as symlinks to the
  /// canonical names in /sys/devices.
  KObjectHandle dev_;

 private:
  /// The block devices, organized by DeviceType.
  ///
  /// Used to populate the /sys/dev/block directory.
  KObjectHandle dev_block_;

  /// The char devices, organized by DeviceType.
  ///
  /// Used to populate the /sys/dev/char directory.
  KObjectHandle dev_char_;

 public:
  // impl KObjectStore

  /// The virtual bus kobject where all virtual and pseudo devices are stored.
  Bus virtual_bus() {
    return Bus(devices_->get_or_create_child<FsNodeOps>(FsString("virtual"), KObjectDirectory::New),
               ktl::nullopt);
  }

  /// The device class used for virtual block devices.
  Class virtual_block_class() { return get_or_create_class("block", virtual_bus()); }

  /// The device class used for virtual graphics devices.
  Class graphics_class() { return get_or_create_class("graphics", virtual_bus()); }

  /// The device class used for virtual input devices.
  Class input_class() { return get_or_create_class("input", virtual_bus()); }

  /// The device class used for virtual mem devices.
  Class mem_class() { return get_or_create_class("mem", virtual_bus()); }

  /// The device class used for virtual misc devices.
  Class misc_class() { return get_or_create_class("misc", virtual_bus()); }

  /// The device class used for virtual tty devices.
  Class tty_class() { return get_or_create_class("tty", virtual_bus()); }

  /// Get a bus by name.
  ///
  /// If the bus does not exist, this function will create it.
  Bus get_or_create_bus(const FsString& name) const {
    auto collection =
        Collection(bus_->get_or_create_child<FsNodeOps>(name, BusCollectionDirectory::New));
    return Bus(devices_->get_or_create_child<FsNodeOps>(name, KObjectDirectory::New), collection);
  }

  /// Get a class by name.
  ///
  /// If the bus does not exist, this function will create it.
  Class get_or_create_class(const FsString& name, Bus bus) const {
    auto collection =
        Collection(class_->get_or_create_child<FsNodeOps>(name, KObjectSymlinkDirectory::New));
    return Class(bus.kobject()->get_or_create_child<FsNodeOps>(name, KObjectDirectory::New), bus,
                 collection);
  }

  /// Create a device and add that device to the store.
  ///
  /// Rather than use this function directly, you should register your device with the
  /// `DeviceRegistry`. The `DeviceRegistry` will create the KObject for the device as
  /// part of the registration process.
  ///
  /// If you create the device yourself, userspace will not be able to instantiate the
  /// device because the `DeviceType` will not be registered with the `DeviceRegistry`.
  template <typename N, typename F>
    requires std::is_convertible_v<std::invoke_result_t<F, Device>, N*> &&
             std::is_base_of_v<FsNodeOps, N>
  Device create_device(const FsStr& name, DeviceMetadata metadata, Class clazz,
                       F&& create_device_sysfs_ops) const {
    auto class_cloned = clazz;
    auto metadata_cloned = metadata;
    auto device_kobject = clazz.kobject()->get_or_create_child<N>(
        name,
        [&create_device_sysfs_ops, class_cloned, metadata_cloned](mtl::WeakPtr<KObject> kobject) {
          return create_device_sysfs_ops(Device(kobject.Lock(), class_cloned, metadata_cloned));
        });

    // Insert the newly created device into various views.
    clazz.collection().kobject()->insert_child(device_kobject);
    // auto device_number = metadata.device_type_.to_string();
    switch (metadata.mode_.type_) {
      case DeviceMode::Type::kBlock:
        block_->insert_child(device_kobject);
        // dev_block_->insert_child_with_name(device_number, device_kobject);
        break;
      case DeviceMode::Type::kChar:
        // dev_char_->insert_child_with_name(device_number, device_kobject);
        break;
    }
    if (auto bus_collection = clazz.bus_.collection_) {
      bus_collection->kobject()->insert_child(device_kobject);
    }

    return Device(device_kobject, clazz, metadata);
  }

  /// Destroy a device.
  ///
  /// This function removes the KObject for the device from the store.
  ///
  /// Most clients hold weak references to KObjects, which means those references will become
  /// invalid shortly after this function is called.
  void destroy_device(const Device& device) const {
    auto kobject = device.kobject();
    auto name = kobject->name();
    // Remove the device from its views in the reverse order in which it was added.
    if (auto bus_collection = device.class_.bus_.collection_) {
      bus_collection->kobject()->remove_child(name);
    }
    // auto device_number = device.metadata_.device_type_.to_string();
    switch (device.metadata_.mode_.type_) {
      case DeviceMode::Type::kBlock:
        // dev_block_->remove_child(device_number);
        block_->remove_child(name);
        break;
      case DeviceMode::Type::kChar:
        // dev_char_->remove_child(device_number);
        break;
    }
    device.class_.collection_.kobject()->remove_child(name);
    // Finally, remove the device from the object store.
    kobject->remove();
  }

 public:
  // impl Default for KObjectStore
  static KObjectStore Default() {
    auto devices = KObject::new_root(FsString(SYSFS_DEVICES));
    auto clazz = KObject::new_root(FsString(SYSFS_CLASS));
    auto block =
        KObject::new_root_with_dir<FsNodeOps>(FsString(SYSFS_BLOCK), KObjectSymlinkDirectory::New);
    auto bus = KObject::new_root(FsString(SYSFS_BUS));
    auto dev = KObject::new_root(FsString(SYSFS_DEV));

    auto dev_block =
        dev->get_or_create_child<FsNodeOps>(FsString("block"), KObjectSymlinkDirectory::New);
    auto dev_char =
        dev->get_or_create_child<FsNodeOps>(FsString("char"), KObjectSymlinkDirectory::New);

    return KObjectStore(ktl::move(devices), ktl::move(clazz), ktl::move(block), ktl::move(bus),
                        ktl::move(dev), ktl::move(dev_block), ktl::move(dev_char));
  }

 private:
  explicit KObjectStore(KObjectHandle devices, KObjectHandle clazz, KObjectHandle block,
                        KObjectHandle bus, KObjectHandle dev, KObjectHandle dev_block,
                        KObjectHandle dev_char)
      : devices_(ktl::move(devices)),
        class_(ktl::move(clazz)),
        block_(ktl::move(block)),
        bus_(ktl::move(bus)),
        dev_(ktl::move(dev)),
        dev_block_(ktl::move(dev_block)),
        dev_char_(ktl::move(dev_char)) {}
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_DEVICE_KOBJECT_STORE_H_
