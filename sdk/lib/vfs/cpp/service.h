// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_VFS_CPP_SERVICE_H_
#define LIB_VFS_CPP_SERVICE_H_

#include <fuchsia/io/cpp/fidl.h>
#include <lib/async/default.h>
#include <lib/fit/function.h>
#include <lib/vfs/cpp/node.h>

namespace vfs {

// A node which binds a channel to a service implementation when opened.
//
// This class is thread-safe.
class Service final : public Node {
 public:
  // Handler callback which binds `channel` to a service instance.
  using Connector = fit::function<void(zx::channel channel, async_dispatcher_t* dispatcher)>;
  explicit Service(Connector connector) : Node(MakeService(std::move(connector))) {}

  template <typename Interface>
  explicit Service(fidl::InterfaceRequestHandler<Interface> handler)
      : Service(
            [handler = std::move(handler)](zx::channel channel, async_dispatcher_t* dispatcher) {
              handler(fidl::InterfaceRequest<Interface>(std::move(channel)));
            }) {}

  using Node::Serve;

 private:
  static vfs_internal_node_t* MakeService(Connector connector) {
    vfs_internal_node_t* svc;
    vfs_internal_svc_context_t context{
        .cookie = new Connector(std::move(connector)),
        .connect = &Connect,
        .destroy = &DestroyCookie,
    };
    ZX_ASSERT(vfs_internal_service_create(&context, &svc) == ZX_OK);
    return svc;
  }

  static void DestroyCookie(void* cookie) { delete static_cast<Connector*>(cookie); }

  static zx_status_t Connect(const void* cookie, zx_handle_t request) {
    (*static_cast<const Connector*>(cookie))(zx::channel{request}, async_get_default_dispatcher());
    return ZX_OK;
  }
};
}  // namespace vfs

#endif  // LIB_VFS_CPP_SERVICE_H_
