// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zircon/types.h>

#ifndef SRC_LIB_UI_WAYLAND_SERVER_CPP_FFI_WAYLAND_SERVER_H_
#define SRC_LIB_UI_WAYLAND_SERVER_CPP_FFI_WAYLAND_SERVER_H_

#ifdef __cplusplus
extern "C" {
#endif

// Opaque pointer to the rust wayland server.
struct wayland_server_handle_t;

// Create and run a new wayland server.
//
// This server will spawn on it's own thread and will start running immediately.
zx_status_t wayland_server_create(wayland_server_handle_t** out);

// Add a new client to the server.
//
// The caller must ensure:
//   * `channel` is a valid handle to a zircon channel.
//   * `server` is a valid pointer, obtained from a call to `wayland_server_create`, and before a
//     call to `wayland_server_destroy`.
void wayland_server_push_client(wayland_server_handle_t* server, zx_handle_t channel);

// Stops a wayland server and frees any resources used for it.
//
// The `server` pointer is no longer valid after this call.
void wayland_server_destroy(wayland_server_handle_t* server);

#ifdef __cplusplus
}
#endif

#endif  // SRC_LIB_UI_WAYLAND_SERVER_CPP_FFI_WAYLAND_SERVER_H_
