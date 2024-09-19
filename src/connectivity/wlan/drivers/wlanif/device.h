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

  // Called by the Rust MLME on the Rust MLME thread when it exits.
  // This is static so that we can pass it to the Rust MLME over the C FFI.
  static void MlmeShutdownCallback(void* self_ptr, zx_status_t status);

  std::unique_ptr<fdf::Logger> logger_;

  // |rust_mlme_| is only accessed by the default dispatcher to start and shutdown MLME in
  // |Device::Start| and |Device::PrepareStop| respectively.
  // Since it's only accessed by the default dispatcher, locking is not required.
  RustFullmacMlme rust_mlme_;

  // The following members manage wlanif::Device shutdown.
  //
  // wlanif::Device can shutdown in one of two ways:
  //
  // 1. The driver framework begins the unbind process due to an external request (e.g., brcmfmac
  //    calls DestroyIface and removes wlanif::Device's node). In this case, wlanif::Device must
  //    stop the Rust MLME before completing shutdown. This is the expected shutdown sequence.
  //
  // 2. The Rust MLME thread has exited before the driver framework has requested
  //    that wlanif::Device shuts down. For example, if the Generic SME channel or
  //    WlanFullmacImplIfc channel is dropped early, then the Rust MLME will exit. When the Rust
  //    MLME exits, it will request that wlanif::Device begins shutting down by calling
  //    `node().reset()`. This is considered an error in the shutdown sequence.

  // |lock_| protects members that are shared between the Rust MLME thread and the default
  // dispatcher thread.
  std::mutex lock_;

  // |prepare_stop_completer_| is populated if |PrepareStop| is called and |mlme_exit_status_| does
  // not contain a value. If |prepare_stop_completer_| contains a value when MLME calls the shutdown
  // callback, it completes the call with its exit status.
  std::optional<fdf::PrepareStopCompleter> prepare_stop_completer_ __TA_GUARDED(lock_);

  // |mlme_exit_status_| is populated if MLME calls its shutdown callback but
  // |prepare_stop_completer_| does not have a value.
  std::optional<zx_status_t> mlme_exit_status_ __TA_GUARDED(lock_);
};

}  // namespace wlanif

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_DEVICE_H_
