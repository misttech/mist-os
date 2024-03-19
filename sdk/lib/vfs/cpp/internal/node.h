// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_VFS_CPP_INTERNAL_NODE_H_
#define LIB_VFS_CPP_INTERNAL_NODE_H_

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

// Represents an object in a file system that communicates via the `fuchsia.io.Node` protocol, and
// potentially supports the composed protocols `fuchsia.io.Directory` and `fuchsia.io.File`.
class Node {
 public:
  virtual ~Node() {
    vfs_internal_node_destroy(handle_);  // Close all connections to this node and destroy it.
  }

  Node(const Node& node) = delete;
  Node& operator=(const Node& node) = delete;
  Node(Node&& node) = delete;
  Node& operator=(Node&& node) = delete;

 protected:
  explicit Node(vfs_internal_node_t* handle) : handle_(handle) { ZX_DEBUG_ASSERT(handle); }

  // Establishes a connection for `request` using the given `flags`. This method must only be used
  // with a single-threaded asynchronous dispatcher. If `dispatcher` is `nullptr`, the current
  // thread's default dispatcher will be used via `async_get_default_dispatcher`.
  //
  // The same `dispatcher` must be used if multiple connections are served for the same node,
  // otherwise `ZX_ERR_INVALID_ARGS` will be returned.
  //
  // Not all node types support `Serve()` due to lifetime restrictions.
  zx_status_t Serve(fuchsia::io::OpenFlags flags, zx::channel request,
                    async_dispatcher_t* dispatcher = nullptr) {
    if (!dispatcher) {
      dispatcher = async_get_default_dispatcher();
    }
    return vfs_internal_node_serve(handle_, dispatcher, request.release(),
                                   static_cast<uint32_t>(flags));
  }

  // Types that require access to `handle()` for operating on child entries.
  friend class vfs::ComposedServiceDir;
  friend class vfs::LazyDir;
  friend class vfs::PseudoDir;

  const vfs_internal_node_t* handle() const { return handle_; }
  vfs_internal_node_t* handle() { return handle_; }

 private:
  vfs_internal_node_t* const handle_;
};

// TODO(https://fxbug.dev/311176363): Deprecate/remove the following type aliases which are used OOT
// and move `Node` to the `vfs` namespace.

using Directory = Node;
using File = Node;

}  // namespace vfs::internal

#endif  // LIB_VFS_CPP_INTERNAL_NODE_H_
