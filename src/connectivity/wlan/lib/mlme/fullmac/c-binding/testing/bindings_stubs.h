// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_LIB_MLME_FULLMAC_C_BINDING_TESTING_BINDINGS_STUBS_H_
#define SRC_CONNECTIVITY_WLAN_LIB_MLME_FULLMAC_C_BINDING_TESTING_BINDINGS_STUBS_H_

#include <functional>

#include "src/connectivity/wlan/lib/mlme/fullmac/c-binding/bindings.h"

// Empty implementation of wlan_fullmac_mlme_handle_t
struct wlan_fullmac_mlme_handle_t {};

namespace bindings_stubs {

using StartFullmacType =
    std::function<wlan_fullmac_mlme_handle_t*(rust_fullmac_device_interface_t)>;
using StopFullmacType = std::function<void(wlan_fullmac_mlme_handle_t*)>;
using DeleteFullmacType = std::function<void(wlan_fullmac_mlme_handle_t*)>;

struct FullmacMlmeBindingsStubs {
  ~FullmacMlmeBindingsStubs();

  static StartFullmacType start_fullmac_mlme_stub;
  static StopFullmacType stop_fullmac_mlme_stub;
  static DeleteFullmacType delete_fullmac_mlme_stub;
};

}  // namespace bindings_stubs
#endif  // SRC_CONNECTIVITY_WLAN_LIB_MLME_FULLMAC_C_BINDING_TESTING_BINDINGS_STUBS_H_
