// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_VFS_CPP_NEW_INTERNAL_NODE_H_
#define LIB_VFS_CPP_NEW_INTERNAL_NODE_H_

#include <fuchsia/io/cpp/fidl.h>
#include <lib/async/default.h>
#include <lib/async/dispatcher.h>
#include <lib/fidl/cpp/interface_request.h>
#include <lib/vfs/internal/libvfs_private.h>

namespace vfs {
// Types that require access to the `handle()` of child entries.
class ComposedServiceDir;
class LazyDir;
class PseudoDir;
}  // namespace vfs

namespace vfs::internal {

// An object in a file system.
//
// Implements the |fuchsia.io.Node| interface. Incoming connections are owned by
// this object and will be destroyed when this object is destroyed.
//
// Subclass to implement a particular kind of file system object.
class Node {
 public:
  virtual ~Node() {
    if (handle_) {
      vfs_internal_node_destroy(handle_);
    }
    if (vfs_handle_) {
      vfs_internal_destroy(vfs_handle_);
    }
  }

  Node(const Node& node) = delete;
  Node& operator=(const Node& node) = delete;
  Node(Node&& node) = delete;
  Node& operator=(Node&& node) = delete;

  // Establishes a connection for |request| using the given |flags|.
  //
  // Waits for messages asynchronously on the |request| channel using
  // |dispatcher|. If |dispatcher| is |nullptr|, the implementation will call
  // |async_get_default_dispatcher| to obtain the default dispatcher for the
  // current thread.
  //
  // This method is NOT thread-safe and must be used with a single-threaded asynchronous dispatcher.
  zx_status_t Serve(fuchsia::io::OpenFlags flags, zx::channel request,
                    async_dispatcher_t* dispatcher = nullptr) {
    if (!vfs_handle_) {
      if (zx_status_t status = vfs_internal_create(
              dispatcher ? dispatcher : async_get_default_dispatcher(), &vfs_handle_);
          status != ZX_OK) {
        return status;
      }
    }
    return vfs_internal_serve(vfs_handle_, handle_, request.release(),
                              static_cast<uint32_t>(flags));
  }

 protected:
  explicit Node(vfs_internal_node_t* handle) : handle_(handle) {}

  // Types that require access to the `handle()` for child entries.
  friend class vfs::ComposedServiceDir;
  friend class vfs::LazyDir;
  friend class vfs::PseudoDir;

  const vfs_internal_node_t* handle() const { return handle_; }
  vfs_internal_node_t* handle() { return handle_; }

 private:
  vfs_internal_node_t* handle_;
  vfs_internal_vfs_t* vfs_handle_ = nullptr;
};

}  // namespace vfs::internal

#endif  // LIB_VFS_CPP_NEW_INTERNAL_NODE_H_
