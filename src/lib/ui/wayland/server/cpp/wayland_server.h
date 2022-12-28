// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/zx/channel.h>
#include <lib/zx/result.h>

#include "src/lib/ui/wayland/server/cpp/ffi/wayland_server.h"

#ifndef SRC_LIB_UI_WAYLAND_SERVER_CPP_WAYLAND_SERVER_H_
#define SRC_LIB_UI_WAYLAND_SERVER_CPP_WAYLAND_SERVER_H_

class WaylandServer {
 public:
  static zx::result<std::unique_ptr<WaylandServer>> Create() {
    wayland_server_handle_t* server;
    zx_status_t status = wayland_server_create(&server);
    if (status != ZX_OK) {
      return zx::error(status);
    }
    return zx::ok(std::unique_ptr<WaylandServer>(new WaylandServer(server)));
  }

  ~WaylandServer() { wayland_server_destroy(server_); }

  void PushClient(zx::channel channel) { wayland_server_push_client(server_, channel.release()); }

 private:
  WaylandServer(wayland_server_handle_t* server) : server_(server) {}

  wayland_server_handle_t* const server_;
};

#endif  // SRC_LIB_UI_WAYLAND_SERVER_CPP_WAYLAND_SERVER_H_
