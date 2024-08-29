// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_DEVICE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_DEVICE_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fuchsia/wlan/mlme/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/sync/cpp/completion.h>
#include <lib/zx/result.h>
#include <zircon/types.h>

#include <memory>

#include <sdk/lib/driver/logging/cpp/logger.h>

#include "src/connectivity/wlan/lib/mlme/fullmac/c-binding/bindings.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}

namespace wlanif {

class Device final : public fdf::DriverBase {
 public:
  explicit Device(fdf::DriverStartArgs start_args,
                  fdf::UnownedSynchronizedDispatcher driver_dispatcher);
  ~Device();

  static constexpr const char* Name() { return "wlanif"; }
  zx::result<> Start() override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  fdf::Logger* Logger() { return logger_.get(); }

 protected:
  void Shutdown();

 private:
  using RustFullmacMlme =
      std::unique_ptr<wlan_fullmac_mlme_handle_t, void (*)(wlan_fullmac_mlme_handle_t*)>;

  std::mutex rust_mlme_lock_;
  RustFullmacMlme rust_mlme_ __TA_GUARDED(rust_mlme_lock_);

  fidl::WireClient<fdf::Node> parent_node_;
  std::unique_ptr<fdf::Logger> logger_;
};

}  // namespace wlanif

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_DEVICE_H_
