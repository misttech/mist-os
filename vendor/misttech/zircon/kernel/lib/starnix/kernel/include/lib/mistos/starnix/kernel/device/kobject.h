// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_DEVICE_KOBJECT_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_DEVICE_KOBJECT_H_

#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/util/btree_map.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <lib/starnix_sync/locks.h>

#include <utility>

#include <fbl/ref_counted_upgradeable.h>
#include <fbl/ref_ptr.h>

namespace starnix {

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
  ktl::optional<util::WeakPtr<KObject>> parent_;

  // A collection of the children of this kobject.
  //
  // The kobject tree has strong references from parent-to-child and weak
  // references from child-to-parent. This will avoid reference cycle.
  mutable starnix_sync::StarnixMutex<util::BTreeMap<FsString, fbl::RefPtr<KObject>>> children_;

  // Function to create the associated FsNodeOps.
  using CreateFsNodeOpsFn = std::function<ktl::unique_ptr<FsNodeOps>(util::WeakPtr<KObject>)>;
  CreateFsNodeOpsFn create_fs_node_ops_;

 public:
  // impl KObject
  static KObjectHandle new_root(const FsString& name);

  template <typename N, typename F>
  static KObjectHandle new_root_with_dir(const FsString& name, F&& create_fs_node_ops);

  template <typename N, typename F>
  static KObjectHandle new_child(const FsString& name, KObjectHandle parent, F&& fn) {
    fbl::AllocChecker ac;
    auto kobject = fbl::AdoptRef(new (&ac) KObject(
        name, util::WeakPtr<KObject>(parent.get()),
        [create_fs_node_ops = ktl::move(fn)](const util::WeakPtr<KObject>& kobject)
            -> ktl::unique_ptr<N> { return ktl::unique_ptr<N>(create_fs_node_ops(kobject)); }));
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
  KObjectHandle get_or_create_child(const FsString& name, F&& create_fs_node_ops) {
    auto guard = children_.Lock();
    auto it = guard->find(name);
    if (it != guard->end()) {
      return it->second;
    }

    auto child = new_child<N>(name, fbl::RefPtr(this), ktl::forward<F>(create_fs_node_ops));
    guard->try_emplace(name, child);
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
  KObject(FsString name, ktl::optional<util::WeakPtr<KObject>> parent, CreateFsNodeOpsFn fn)
      : name_(ktl::move(name)),
        parent_(ktl::move(parent)),
        children_(),
        create_fs_node_ops_(ktl::move(fn)) {}
};

using KObjectHandle = fbl::RefPtr<KObject>;

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_DEVICE_KOBJECT_H_
