// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_VFS_CPP_NEW_PSEUDO_DIR_H_
#define LIB_VFS_CPP_NEW_PSEUDO_DIR_H_

#include <fuchsia/io/cpp/fidl.h>
#include <lib/vfs/cpp/new/internal/node.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>

#include <map>
#include <memory>
#include <mutex>
#include <string>

namespace vfs {
class PseudoDir final : public internal::Node {
 public:
  PseudoDir() : internal::Node(CreateDirectory()) {}

  // Adds a directory entry associating the given |name| with |vn|.
  // It is ok to add the same Node multiple times with different names.
  //
  // Returns |ZX_OK| on success.
  // Returns |ZX_ERR_ALREADY_EXISTS| if there is already a node with the given
  // name.
  zx_status_t AddSharedEntry(std::string name, std::shared_ptr<Node> vn) {
    std::lock_guard guard(mutex_);
    if (node_map_.find(name) != node_map_.cend()) {
      return ZX_ERR_ALREADY_EXISTS;
    }
    if (zx_status_t status = vfs_internal_directory_add(handle(), vn->handle(), name.c_str());
        status != ZX_OK) {
      return status;
    }
    node_map_.emplace(std::move(name), std::move(vn));
    return ZX_OK;
  }

  // Adds a directory entry associating the given |name| with |vn|.
  //
  // Returns |ZX_OK| on success.
  // Returns |ZX_ERR_ALREADY_EXISTS| if there is already a node with the given
  // name.
  zx_status_t AddEntry(std::string name, std::unique_ptr<Node> vn) {
    return AddSharedEntry(std::move(name), std::move(vn));
  }

  // Removes a directory entry with the given |name|.
  //
  // Returns |ZX_OK| on success.
  // Returns |ZX_ERR_NOT_FOUND| if there is no node with the given name.
  zx_status_t RemoveEntry(const std::string& name) { return RemoveEntryImpl(name, nullptr); }

  // Removes a directory entry with the given |name| and |node|.
  //
  // Returns |ZX_OK| on success.
  // Returns |ZX_ERR_NOT_FOUND| if there is no node with |name| that matches |node|.
  // TODO(https://fxbug.dev/293936429): Verify `node`.
  zx_status_t RemoveEntry(const std::string& name, const Node* node) {
    return RemoveEntryImpl(name, node);
  }

  // Checks if directory is empty. Use caution if modifying this directory from multiple threads.
  bool IsEmpty() const {
    std::lock_guard guard(mutex_);
    return node_map_.empty();
  }

  // |vfs::internal::Node| implementation

  zx_status_t Lookup(std::string_view name, Node** out_node) const override {
    std::lock_guard guard(mutex_);
    if (auto node_it = node_map_.find(name); node_it != node_map_.cend()) {
      *out_node = node_it->second.get();
      return ZX_OK;
    }
    return ZX_ERR_NOT_FOUND;
  }

  bool IsDirectory() const override { return true; }

 private:
  static inline vfs_internal_node_t* CreateDirectory() {
    vfs_internal_node_t* dir;
    ZX_ASSERT(vfs_internal_directory_create(&dir) == ZX_OK);
    return dir;
  }

  zx_status_t RemoveEntryImpl(const std::string& name, const Node* node) {
    std::lock_guard guard(mutex_);
    if (auto found = node_map_.find(name); found != node_map_.cend()) {
      if (node && node != found->second.get()) {
        return ZX_ERR_NOT_FOUND;
      }
      if (zx_status_t status = vfs_internal_directory_remove(handle(), name.c_str());
          status != ZX_OK) {
        return status;
      }
      node_map_.erase(found);
      return ZX_OK;
    }
    return ZX_ERR_NOT_FOUND;
  }

  mutable std::mutex mutex_;

  // *NOTE*: Due to the SDK VFS `Lookup()` semantics, we need to maintain a strong reference to the
  // nodes added to this directory. `Lookup()` returns a `Node*` which callers then downcast to the
  // concrete node type. The underlying implementation of the pseudo-directory has no concept of
  // the C++ types specified here, so we must store them here to allow safe downcasting.
  //
  // TODO(https://fxbug.dev/311176363): `Lookup()` *should* be handled by the in-tree VFS code, but
  // this requires changing `Lookup()` to return a concrete type of node specified by the caller.
  // This implies that the C++ types defined here should act more as handle-based types that can be
  // copied/moved. We might also consider removing `Lookup()` and require callers maintain
  // references/pointers for the nodes they wish to keep track of.
  std::map<std::string, std::shared_ptr<internal::Node>, std::less<>> node_map_ __TA_GUARDED(
      mutex_);
};
}  // namespace vfs

#endif  // LIB_VFS_CPP_NEW_PSEUDO_DIR_H_
