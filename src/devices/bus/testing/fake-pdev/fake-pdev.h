// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_DEVICES_BUS_TESTING_FAKE_PDEV_FAKE_PDEV_H_
#define SRC_DEVICES_BUS_TESTING_FAKE_PDEV_FAKE_PDEV_H_

#include <fidl/fuchsia.hardware.platform.device/cpp/wire_test_base.h>
#include <fuchsia/hardware/platform/device/cpp/banjo.h>
#include <lib/async/default.h>
#include <lib/mmio/mmio.h>

#include <atomic>
#include <map>
#include <optional>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>

namespace fake_pdev {

struct MmioInfo {
  zx::vmo vmo;
  zx_off_t offset;
  size_t size;
};

using Mmio = std::variant<MmioInfo, fdf::MmioBuffer>;

class FakePDevFidl : public fidl::WireServer<fuchsia_hardware_platform_device::Device> {
 public:
  struct Config {
    // If true, a bti will be generated lazily if it does not exist.
    bool use_fake_bti = false;

    // If true, a smc will be generated lazily if it does not exist.
    bool use_fake_smc = false;

    // If true, an irq will be generated lazily if it does not exist.
    bool use_fake_irq = false;

    std::map<uint32_t, Mmio> mmios;
    std::map<uint32_t, zx::interrupt> irqs;
    std::map<uint32_t, zx::bti> btis;
    std::map<uint32_t, zx::resource> smcs;

    std::optional<pdev_device_info_t> device_info;
    std::optional<pdev_board_info_t> board_info;
  };

  FakePDevFidl() = default;

  fuchsia_hardware_platform_device::Service::InstanceHandler GetInstanceHandler() {
    return fuchsia_hardware_platform_device::Service::InstanceHandler({
        .device = binding_group_.CreateHandler(this, async_get_default_dispatcher(),
                                               fidl::kIgnoreBindingClosure),
    });
  }

  zx_status_t Connect(fidl::ServerEnd<fuchsia_hardware_platform_device::Device> request) {
    binding_group_.AddBinding(async_get_default_dispatcher(), std::move(request), this,
                              fidl::kIgnoreBindingClosure);
    return ZX_OK;
  }

  zx_status_t SetConfig(Config config) {
    config_ = std::move(config);
    return ZX_OK;
  }

 private:
  void GetMmio(GetMmioRequestView request, GetMmioCompleter::Sync& completer) override;
  void GetInterrupt(GetInterruptRequestView request,
                    GetInterruptCompleter::Sync& completer) override;
  void GetBti(GetBtiRequestView request, GetBtiCompleter::Sync& completer) override;
  void GetSmc(GetSmcRequestView request, GetSmcCompleter::Sync& completer) override;
  void GetDeviceInfo(GetDeviceInfoCompleter::Sync& completer) override;
  void GetBoardInfo(GetBoardInfoCompleter::Sync& completer) override;

  Config config_;
  fidl::ServerBindingGroup<fuchsia_hardware_platform_device::Device> binding_group_;
};

}  // namespace fake_pdev

#endif  // SRC_DEVICES_BUS_TESTING_FAKE_PDEV_FAKE_PDEV_H_
