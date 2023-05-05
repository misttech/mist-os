// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.wlan.device/cpp/wire.h>
#include <fidl/fuchsia.wlan.tap/cpp/fidl.h>
#include <fidl/fuchsia.wlan.tap/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/dispatcher.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <zircon/fidl.h>
#include <zircon/status.h>

#include <memory>
#include <mutex>

#include <ddktl/fidl.h>
#include <src/lib/fidl/cpp/include/lib/fidl/cpp/wire_natural_conversions.h>

#include "wlantap-phy.h"

namespace {

namespace wlan_tap = fuchsia_wlan_tap::wire;

// Max size of WlantapPhyConfig.
constexpr size_t kWlantapPhyConfigBufferSize =
    fidl::MaxSizeInChannel<wlan_tap::WlantapPhyConfig, fidl::MessageDirection::kSending>();

fidl::Arena<kWlantapPhyConfigBufferSize> phy_config_arena;

struct WlantapCtl : fidl::WireServer<fuchsia_wlan_tap::WlantapCtl> {
  WlantapCtl() = default;

  static void DdkRelease(void* ctx) { delete static_cast<WlantapCtl*>(ctx); }

  void CreatePhy(CreatePhyRequestView request, CreatePhyCompleter::Sync& completer) override {
    phy_config_arena.Reset();

    auto natural_config = fidl::ToNatural(request->config);
    auto wire_config = fidl::ToWire(phy_config_arena, std::move(natural_config));
    auto phy_config = std::make_shared<wlan_tap::WlantapPhyConfig>(wire_config);

    completer.Reply(wlan::CreatePhy(device_, request->proxy.TakeChannel(), phy_config));
  }

  static void DdkMessage(void* ctx, fidl_incoming_msg_t msg, device_fidl_txn_t txn) {
    auto self = static_cast<WlantapCtl*>(ctx);

    fidl::WireDispatch<fuchsia_wlan_tap::WlantapCtl>(
        self, fidl::IncomingHeaderAndMessage::FromEncodedCMessage(msg),
        ddk::FromDeviceFIDLTransaction(txn));
  }

  zx_device_t* device_ = nullptr;
};

}  // namespace

zx_status_t wlantapctl_bind(void* ctx, zx_device_t* parent) {
  auto wlantapctl = std::make_unique<WlantapCtl>();
  static zx_protocol_device_t device_ops = {
      .version = DEVICE_OPS_VERSION,
      .release = &WlantapCtl::DdkRelease,
      .message = &WlantapCtl::DdkMessage,
  };
  device_add_args_t args = {
      .version = DEVICE_ADD_ARGS_VERSION,
      .name = "wlantapctl",
      .ctx = wlantapctl.get(),
      .ops = &device_ops,
      .flags = DEVICE_ADD_NON_BINDABLE,
  };
  zx_status_t status = device_add(parent, &args, &wlantapctl->device_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: could not add device: %d", __func__, status);
    return status;
  }
  // Transfer ownership to devmgr
  [[maybe_unused]] WlantapCtl* ptr = wlantapctl.release();
  return ZX_OK;
}

static constexpr zx_driver_ops_t wlantapctl_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = wlantapctl_bind;
  return ops;
}();

ZIRCON_DRIVER(wlantapctl, wlantapctl_driver_ops, "fuchsia", "0.1");
