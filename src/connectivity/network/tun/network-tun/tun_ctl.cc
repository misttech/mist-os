// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tun_ctl.h"

#include <lib/async/cpp/task.h>
#include <lib/fdf/cpp/env.h>
#include <lib/fit/defer.h>
#include <lib/syslog/global.h>
#include <zircon/status.h>

#include "tun_device.h"

namespace network {
namespace tun {

zx::result<std::unique_ptr<TunCtl>> TunCtl::Create(async_dispatcher_t* fidl_dispatcher) {
  std::unique_ptr<TunCtl> tun_ctl(new TunCtl(fidl_dispatcher));

  zx::result dispatchers = network::OwnedDeviceInterfaceDispatchers::Create();
  if (dispatchers.is_error()) {
    FX_LOGF(ERROR, "tun", "failed to create owned dispatchers: %s", dispatchers.status_string());
    return dispatchers.take_error();
  }
  tun_ctl->dispatchers_ = std::move(dispatchers.value());

  zx::result shim_dispatchers = network::OwnedShimDispatchers::Create();
  if (shim_dispatchers.is_error()) {
    FX_LOGF(ERROR, "tun", "failed to create owned shim dispatchers: %s",
            shim_dispatchers.status_string());
    return shim_dispatchers.take_error();
  }
  tun_ctl->shim_dispatchers_ = std::move(shim_dispatchers.value());

  return zx::ok(std::move(tun_ctl));
}

TunCtl::~TunCtl() {
  if (dispatchers_) {
    dispatchers_->ShutdownSync();
  }
  if (shim_dispatchers_) {
    shim_dispatchers_->ShutdownSync();
  }
}

void TunCtl::CreateDevice(CreateDeviceRequestView request, CreateDeviceCompleter::Sync& completer) {
  zx::result tun_device = TunDevice::Create(
      dispatchers_->Unowned(), shim_dispatchers_->Unowned(),
      [this](TunDevice* dev) {
        // If this is posted on fdf_dispatcher then there's a lockup because we're
        // then creating a double lock in DevicePort.
        async::PostTask(fidl_dispatcher_, [this, dev]() {
          devices_.erase(*dev);
          TryFireShutdownCallback();
        });
      },
      DeviceConfig(request->config));

  if (tun_device.is_error()) {
    FX_LOGF(ERROR, "tun", "TunCtl: TunDevice creation failed: %s", tun_device.status_string());
    request->device.Close(tun_device.error_value());
    return;
  }
  auto& value = tun_device.value();
  value->Bind(std::move(request->device));
  devices_.push_back(std::move(value));
  FX_LOG(INFO, "tun", "TunCtl: Created TunDevice");
}

void TunCtl::CreatePair(CreatePairRequestView request, CreatePairCompleter::Sync& completer) {
  zx::result tun_pair = TunPair::Create(
      dispatchers_->Unowned(), shim_dispatchers_->Unowned(), fidl_dispatcher_,
      [this](TunPair* pair) {
        async::PostTask(fidl_dispatcher_, [this, pair]() {
          device_pairs_.erase(*pair);
          TryFireShutdownCallback();
        });
      },
      DevicePairConfig(request->config));

  if (tun_pair.is_error()) {
    FX_LOGF(ERROR, "tun", "TunCtl: TunPair creation failed: %s", tun_pair.status_string());
    request->device_pair.Close(tun_pair.status_value());
    return;
  }
  auto& value = tun_pair.value();
  value->Bind(std::move(request->device_pair));
  device_pairs_.push_back(std::move(value));
  FX_LOG(INFO, "tun", "TunCtl: Created TunPair");
}

void TunCtl::SetSafeShutdownCallback(fit::callback<void()> shutdown_callback) {
  async::PostTask(fidl_dispatcher_, [this, callback = std::move(shutdown_callback)]() mutable {
    ZX_ASSERT_MSG(!shutdown_callback_, "Shutdown callback already installed");
    shutdown_callback_ = std::move(callback);
    TryFireShutdownCallback();
  });
}

void TunCtl::TryFireShutdownCallback() {
  if (shutdown_callback_ && device_pairs_.is_empty() && devices_.is_empty()) {
    shutdown_callback_();
  }
}

}  // namespace tun
}  // namespace network
