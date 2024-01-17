// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_VFS_CPP_NEW_COMPOSED_SERVICE_DIR_H_
#define LIB_VFS_CPP_NEW_COMPOSED_SERVICE_DIR_H_

#include <fuchsia/io/cpp/fidl.h>
#include <lib/vfs/cpp/new/internal/node.h>
#include <lib/vfs/cpp/new/service.h>
#include <zircon/assert.h>

#include <string>
#include <string_view>

namespace vfs {

// A directory-like object which created a composed PseudoDir on top of
// |fallback_dir|.It can be used to connect to services in |fallback_dir| but it
// will not enumerate them.
//
// TODO(https://fxbug.dev/309685624): Remove when all callers have migrated.
class ComposedServiceDir final : public internal::Node {
 public:
  ComposedServiceDir() : Node(MakeComposedServiceDir()) {}

  void set_fallback(fidl::InterfaceHandle<fuchsia::io::Directory> fallback_dir) {
    ZX_ASSERT(vfs_internal_composed_svc_dir_set_fallback(
                  handle(), fallback_dir.TakeChannel().release()) == ZX_OK);
  }

  void AddService(const std::string& service_name, std::unique_ptr<vfs::Service> service) {
    ZX_ASSERT(vfs_internal_composed_svc_dir_add(handle(), service->handle(), service_name.data()) ==
              ZX_OK);
  }

  zx_status_t Lookup(std::string_view name, vfs::internal::Node** out_node) const override {
    // No existing callers require Lookup functionality for this node type.
    ZX_PANIC("TODO(https://fxbug.dev/309685624)");
  }

 private:
  static inline vfs_internal_node_t* MakeComposedServiceDir() {
    vfs_internal_node_t* dir;
    ZX_ASSERT(vfs_internal_composed_svc_dir_create(&dir) == ZX_OK);
    return dir;
  }

} ZX_DEPRECATED_SINCE(
    1, 16,
    "Create and serve a custom outgoing directory with //sdk/lib/component or //sdk/lib/svc.");

}  // namespace vfs

#endif  // LIB_VFS_CPP_NEW_COMPOSED_SERVICE_DIR_H_
