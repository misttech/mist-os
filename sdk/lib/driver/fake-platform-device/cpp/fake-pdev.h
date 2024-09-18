// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_FAKE_PLATFORM_DEVICE_CPP_FAKE_PDEV_H_
#define LIB_DRIVER_FAKE_PLATFORM_DEVICE_CPP_FAKE_PDEV_H_

#include <fidl/fuchsia.hardware.platform.device/cpp/wire_test_base.h>
#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <lib/async/default.h>
#include <lib/driver/platform-device/cpp/pdev.h>
#include <lib/mmio/mmio.h>

#include <map>

namespace fdf_fake_platform_device {

using Mmio = std::variant<fdf::PDev::MmioInfo, fdf::MmioBuffer>;

class FakePDev : public fidl::WireServer<fuchsia_hardware_platform_device::Device> {
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

    std::optional<fdf::PDev::DeviceInfo> device_info;
    std::optional<fdf::PDev::BoardInfo> board_info;
    std::vector<fuchsia_hardware_power::PowerElementConfiguration> power_elements;
  };

  FakePDev() = default;

  fuchsia_hardware_platform_device::Service::InstanceHandler GetInstanceHandler(
      async_dispatcher_t* dispatcher = nullptr) {
    return fuchsia_hardware_platform_device::Service::InstanceHandler({
        .device = binding_group_.CreateHandler(
            this, dispatcher ? dispatcher : async_get_default_dispatcher(),
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

  void set_metadata(std::unordered_map<uint32_t, std::vector<uint8_t>> metadata) {
    metadata_ = std::move(metadata);
  }

 private:
  void GetMmioById(GetMmioByIdRequestView request, GetMmioByIdCompleter::Sync& completer) override;
  void GetMmioByName(GetMmioByNameRequestView request,
                     GetMmioByNameCompleter::Sync& completer) override;
  void GetInterruptById(GetInterruptByIdRequestView request,
                        GetInterruptByIdCompleter::Sync& completer) override;
  void GetInterruptByName(GetInterruptByNameRequestView request,
                          GetInterruptByNameCompleter::Sync& completer) override;
  void GetBtiById(GetBtiByIdRequestView request, GetBtiByIdCompleter::Sync& completer) override;
  void GetBtiByName(GetBtiByNameRequestView request,
                    GetBtiByNameCompleter::Sync& completer) override;
  void GetSmcById(GetSmcByIdRequestView request, GetSmcByIdCompleter::Sync& completer) override;
  void GetSmcByName(GetSmcByNameRequestView request,
                    GetSmcByNameCompleter::Sync& completer) override;
  void GetNodeDeviceInfo(GetNodeDeviceInfoCompleter::Sync& completer) override;
  void GetBoardInfo(GetBoardInfoCompleter::Sync& completer) override;
  void GetPowerConfiguration(GetPowerConfigurationCompleter::Sync& completer) override;
  void GetMetadata(GetMetadataRequestView request, GetMetadataCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_platform_device::Device> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  Config config_;
  fidl::ServerBindingGroup<fuchsia_hardware_platform_device::Device> binding_group_;
  std::unordered_map<uint32_t, std::vector<uint8_t>> metadata_;
};

}  // namespace fdf_fake_platform_device

#endif  // LIB_DRIVER_FAKE_PLATFORM_DEVICE_CPP_FAKE_PDEV_H_
