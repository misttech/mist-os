// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_FULLMAC_MLME_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_FULLMAC_MLME_H_

#include <zircon/types.h>

#include <memory>

#include "src/connectivity/wlan/lib/mlme/fullmac/c-binding/bindings.h"

namespace wlanif {

using RustFullmacMlme =
    std::unique_ptr<wlan_fullmac_mlme_handle_t, void (*)(wlan_fullmac_mlme_handle_t*)>;

class Device;

class FullmacMlme {
 public:
  explicit FullmacMlme(Device* device);
  ~FullmacMlme() = default;
  zx_status_t Init();
  void StopMainLoop();

 private:
  Device* device_;
  RustFullmacMlme rust_mlme_;
};

}  // namespace wlanif

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_FULLMAC_MLME_H_
