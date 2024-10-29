// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_VFS_CPP_COMPOSED_SERVICE_DIR_H_
#define LIB_VFS_CPP_COMPOSED_SERVICE_DIR_H_

#include <fidl/fuchsia.io/cpp/markers.h>
#include <fuchsia/io/cpp/fidl.h>
#include <lib/fidl/cpp/client.h>
#include <lib/vfs/cpp/node.h>
#include <lib/vfs/cpp/service.h>
#include <zircon/assert.h>

#include <string>

namespace vfs {

// A directory-like object which created a composed `PseudoDir` on top of a `fallback_dir`. It can
// be used to connect to services in `fallback_dir`, but will not enumerate them.
//
// This class is thread-safe.
class ComposedServiceDir final : public Node {
 public:
  ComposedServiceDir() : Node(MakeComposedServiceDir()) {}

  using Node::Serve;

  // Sets the fallback directory for services. Services in this directory can be connected to, but
  // will not be enumerated. This method may only be called once.
  void SetFallback(fidl::ClientEnd<fuchsia_io::Directory> fallback_dir) {
    ZX_ASSERT(vfs_internal_composed_svc_dir_set_fallback(
                  handle(), fallback_dir.TakeChannel().release()) == ZX_OK);
  }

  // Adds a service to this directory. Services added will be preferred over those in the fallback
  // directory, and can be enumerated.
  void AddService(const std::string& service_name, std::unique_ptr<vfs::Service> service) {
    ZX_ASSERT(vfs_internal_composed_svc_dir_add(handle(), service->handle(), service_name.data()) ==
              ZX_OK);
  }

  // * * * * * * * * * * * * * * * * * * * * * * * * * * * * * * * * * * * * * * * *
  // Deprecated HLCPP Signatures
  // * * * * * * * * * * * * * * * * * * * * * * * * * * * * * * * * * * * * * * * *
  //
  // TODO(https://fxbug.dev/336617685): Mark the following signatures as deprecated once all callers
  // have migratred to the above LLCPP signatures.

  // Sets the fallback directory for services. Services in this directory can be connected to, but
  // will not be enumerated. This method may only be called once.
  void set_fallback(fidl::InterfaceHandle<fuchsia::io::Directory> fallback_dir)
      ZX_DEPRECATED_SINCE(1, 16, "Use SetFallback instead.") {
    ZX_ASSERT(vfs_internal_composed_svc_dir_set_fallback(
                  handle(), fallback_dir.TakeChannel().release()) == ZX_OK);
  }

 private:
  static vfs_internal_node_t* MakeComposedServiceDir() {
    vfs_internal_node_t* dir;
    ZX_ASSERT(vfs_internal_composed_svc_dir_create(&dir) == ZX_OK);
    return dir;
  }
};

}  // namespace vfs

#endif  // LIB_VFS_CPP_COMPOSED_SERVICE_DIR_H_
