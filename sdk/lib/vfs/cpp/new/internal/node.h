// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_VFS_CPP_NEW_INTERNAL_NODE_H_
#define LIB_VFS_CPP_NEW_INTERNAL_NODE_H_

#include <fuchsia/io/cpp/fidl.h>
#include <lib/async/default.h>
#include <lib/async/dispatcher.h>
#include <lib/fidl/cpp/interface_request.h>
#include <lib/vfs/cpp/new/internal/libvfs_private.h>

namespace vfs {
// Types that require access to the `handle()` for adding child node entries.
class PseudoDir;
class ComposedServiceDir;
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

  // Find an entry in this directory with the given |name|.
  //
  // The entry is returned via |out_node|. The returned entry is owned by this
  // directory.
  //
  // Returns |ZX_ERR_NOT_FOUND| if no entry exists.
  // Default implementation in this class return |ZX_ERR_NOT_DIR| if
  // |IsDirectory| is false, else throws error with |ZX_ASSERT|.
  //
  // All directory types which are not remote should implement this method.
  //
  // TODO(https://fxbug.dev/293936429): Make Lookup a non-virtual method of PseudoDir once LazyDir
  // is removed, or move it to a separate Directory interface.
  virtual zx_status_t Lookup(std::string_view name, Node** out_node) const {
    return ZX_ERR_NOT_DIR;
  }

  // Return true if |Node| is a remote node.
  // TODO(https://fxbug.dev/293936429): Deprecate this method, it should not be required for
  // protocol resolution.
  virtual bool IsRemote() const { return false; }

  // Return true if |Node| is a directory.
  // TODO(https://fxbug.dev/293936429): Deprecate this method, it should not be required for
  // protocol resolution.
  virtual bool IsDirectory() const { return false; }

 protected:
  explicit Node(vfs_internal_node_t* handle) : handle_(handle) {}

  // Types that require access to the `handle()` for adding child node entries.
  friend class vfs::PseudoDir;
  friend class vfs::ComposedServiceDir;

  const vfs_internal_node_t* handle() const { return handle_; }
  vfs_internal_node_t* handle() { return handle_; }

 private:
  vfs_internal_node_t* handle_;
  vfs_internal_vfs_t* vfs_handle_ = nullptr;
};

}  // namespace vfs::internal

#endif  // LIB_VFS_CPP_NEW_INTERNAL_NODE_H_
