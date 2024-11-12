// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_DEVICE_KOBJECT_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_DEVICE_KOBJECT_H_

#include <lib/mistos/memory/weak_ptr.h>
#include <lib/mistos/starnix/kernel/device/device_mode.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/device_type.h>
#include <lib/mistos/util/btree_map.h>
#include <lib/starnix_sync/locks.h>

#include <utility>

#include <fbl/ref_counted_upgradeable.h>
#include <fbl/ref_ptr.h>

namespace starnix {

using starnix_uapi::DeviceType;

class FsNodeOps;

/// A kobject is the fundamental unit of the sysfs /devices subsystem. Each kobject represents a
/// sysfs object.
///
/// A kobject has a name, a function to create FsNodeOps, pointers to its children, and a pointer
/// to its parent, which allows it to be organized into hierarchies.
class KObject : public fbl::RefCountedUpgradeable<KObject> {
 private:
  using KObjectHandle = fbl::RefPtr<KObject>;

  // The name that will appear in sysfs.
  //
  // It is also used by the parent to find this child. This name will be reflected in the full
  // path from the root.
  FsString name_;

  // The weak reference to its parent kobject.
  ktl::optional<mtl::WeakPtr<KObject>> parent_;

  // A collection of the children of this kobject.
  //
  // The kobject tree has strong references from parent-to-child and weak
  // references from child-to-parent. This will avoid reference cycle.
  mutable starnix_sync::Mutex<util::BTreeMap<FsString, fbl::RefPtr<KObject>>> children_;

  // Function to create the associated FsNodeOps.
  using CreateFsNodeOpsFn = std::function<ktl::unique_ptr<FsNodeOps>(mtl::WeakPtr<KObject>)>;
  CreateFsNodeOpsFn create_fs_node_ops_;

 public:
  // impl KObject
  static KObjectHandle new_root(const FsString& name);

  template <typename N, typename F>
    requires std::is_convertible_v<std::invoke_result_t<F, mtl::WeakPtr<KObject>>, N*> &&
             std::is_base_of_v<FsNodeOps, N>
  static KObjectHandle new_root_with_dir(const FsString& name, F&& create_fs_node_ops) {
    fbl::AllocChecker ac;
    auto kobject = fbl::AdoptRef(new (&ac) KObject(
        name, {}, [&](const mtl::WeakPtr<KObject>& kobject) -> ktl::unique_ptr<N> {
          return ktl::unique_ptr<N>(create_fs_node_ops(kobject));
        }));
    ZX_ASSERT(ac.check());

    return kobject;
  }

  template <typename N, typename F>
    requires std::is_convertible_v<std::invoke_result_t<F, mtl::WeakPtr<KObject>>, N*> &&
             std::is_base_of_v<FsNodeOps, N>
  static KObjectHandle new_child(const FsString& name, KObjectHandle parent,
                                 F&& create_fs_node_ops) {
    fbl::AllocChecker ac;
    auto kobject = fbl::AdoptRef(
        new (&ac) KObject(name, parent->weak_factory_.GetWeakPtr(),
                          [&](const mtl::WeakPtr<KObject>& kobject) -> ktl::unique_ptr<N> {
                            return ktl::unique_ptr<N>(create_fs_node_ops(kobject));
                          }));
    ZX_ASSERT(ac.check());

    return kobject;
  }

  // The name that will appear in sysfs.
  const FsString& name() const { return name_; }

  // The parent kobject.
  //
  // Returns none if this kobject is the root.
  ktl::optional<KObjectHandle> parent() const;

  // Returns the associated `FsNodeOps`.
  //
  // The `create_fs_node_ops` function will be called with a weak pointer to kobject itself.
  ktl::unique_ptr<FsNodeOps> ops();

  /// Get the path to the current kobject, relative to the root.
  FsString path();

  // Get the path to the root, relative to the current kobject.
  FsString path_to_root() const;

  // Checks if there is any child holding the `name`.
  bool has_child(const FsString& name) const;

  // Get the child based on the name.
  ktl::optional<KObjectHandle> get_child(const FsString& name) const;

  // Get or create a child with the given name and create_fs_node_ops function.
  template <typename N, typename F>
    requires std::is_convertible_v<std::invoke_result_t<F, mtl::WeakPtr<KObject>>, N*> &&
             std::is_base_of_v<FsNodeOps, N>
  KObjectHandle get_or_create_child(const FsString& name, F&& create_fs_node_ops) {
    auto guard = children_.Lock();
    auto it = guard->find(name);
    if (it != guard->end()) {
      return it->second;
    }

    auto child = new_child<N>(name, fbl::RefPtr(this), create_fs_node_ops);
    auto [_, inserted] = guard->try_emplace(name, child);
    ZX_ASSERT_MSG(inserted, "Failed to insert child kobject");
    return child;
  }

  void insert_child(KObjectHandle child);

  void insert_child_with_name(const FsString& name, KObjectHandle child);

  // Collects all children names.
  fbl::Vector<FsString> get_children_names() const;

  fbl::Vector<KObjectHandle> get_children_kobjects() const;

  // Removes the child if exists.
  ktl::optional<std::pair<FsString, KObjectHandle>> remove_child(const FsString& name);

  // Removes itself from the parent kobject.
  void remove();

 private:
  KObject(FsString name, ktl::optional<mtl::WeakPtr<KObject>> parent, CreateFsNodeOpsFn fn)
      : name_(ktl::move(name)),
        parent_(ktl::move(parent)),
        create_fs_node_ops_(ktl::move(fn)),
        weak_factory_(this) {}

 public:
  mtl::WeakPtrFactory<KObject> weak_factory_;  // must be last
};

using KObjectHandle = fbl::RefPtr<KObject>;

// A trait implemented by all kobject-based types
class KObjectBased {
 public:
  virtual ~KObjectBased() = default;
  virtual KObjectHandle kobject() const = 0;
};

// Implements the KObjectBased trait for a KObject new type class
#define IMPL_KOBJECT_BASED(Name)                               \
 public:                                                       \
  KObjectHandle kobject() const override {                     \
    auto ptr = kobject_.Lock();                                \
    ZX_ASSERT_MSG(ptr, "Embedded kobject has been droppped."); \
    return ptr;                                                \
  }                                                            \
                                                               \
  using __force_semicolon_##Name = int

// A collection of devices whose `parent` kobject is not the embedded kobject.
//
// Used for grouping devices in the sysfs subsystem.
class Collection : public KObjectBased {
 private:
  mtl::WeakPtr<KObject> kobject_;

 public:
  explicit Collection(KObjectHandle kobject) : kobject_(kobject->weak_factory_.GetWeakPtr()) {}

  IMPL_KOBJECT_BASED(Collection);
};

// A Bus identifies how the devices are connected to the processor.
class Bus : public KObjectBased {
 private:
  mtl::WeakPtr<KObject> kobject_;

 public:
  ktl::optional<Collection> collection_;

 public:
  Bus(KObjectHandle kobject, ktl::optional<Collection> collection)
      : kobject_(kobject->weak_factory_.GetWeakPtr()), collection_(ktl::move(collection)) {}

  IMPL_KOBJECT_BASED(Bus);
};

// A Class is a higher-level view of a device.
//
// It groups devices based on what they do, rather than how they are connected.
class Class : public KObjectBased {
 private:
  mtl::WeakPtr<KObject> kobject_;

 public:
  /// Physical bus that the devices belong to
  Bus bus_;
  Collection collection_;

 public:
  Class(KObjectHandle kobject, Bus bus, Collection collection)
      : kobject_(kobject->weak_factory_.GetWeakPtr()),
        bus_(ktl::move(bus)),
        collection_(ktl::move(collection)) {}

  // Physical bus that the devices belong to.
  Bus bus() const { return bus_; }
  Collection collection() const { return collection_; }

  IMPL_KOBJECT_BASED(Class);
};

class DeviceMetadata {
 public:
  // Name of the device in /dev.
  ///
  /// Also appears in sysfs via uevent.
  FsString devname_;
  DeviceType device_type_;
  DeviceMode mode_;

 public:
  DeviceMetadata(FsString devname, DeviceType device_type, DeviceMode mode)
      : devname_(ktl::move(devname)), device_type_(device_type), mode_(mode) {}
};

class Device : public KObjectBased {
 public:
  mtl::WeakPtr<KObject> kobject_;
  /// Class kobject that the device belongs to.
  Class class_;
  DeviceMetadata metadata_;

  Device(KObjectHandle kobject, Class class_, DeviceMetadata metadata)
      : kobject_(kobject->weak_factory_.GetWeakPtr()),
        class_(ktl::move(class_)),
        metadata_(ktl::move(metadata)) {}

  IMPL_KOBJECT_BASED(Device);
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_DEVICE_KOBJECT_H_
