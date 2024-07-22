// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/wlan/lib/mlme/fullmac/c-binding/bindings.h"

struct wlan_fullmac_mlme_handle_t {};

extern "C" wlan_fullmac_mlme_handle_t *start_fullmac_mlme(
    rust_fullmac_device_interface_t raw_device) {
  static wlan_fullmac_mlme_handle_t handle;
  return &handle;
}

extern "C" void stop_fullmac_mlme(wlan_fullmac_mlme_handle_t *mlme) {}
extern "C" void delete_fullmac_mlme(wlan_fullmac_mlme_handle_t *mlme) {}

