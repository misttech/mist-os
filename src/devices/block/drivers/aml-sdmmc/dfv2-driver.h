// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_AML_SDMMC_DFV2_DRIVER_H_
#define SRC_DEVICES_BLOCK_DRIVERS_AML_SDMMC_DFV2_DRIVER_H_

#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.sdmmc/cpp/driver/fidl.h>
#include <fuchsia/hardware/sdmmc/cpp/banjo.h>
#include <lib/ddk/metadata.h>
#include <lib/dma-buffer/buffer.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/inspect/component/cpp/component.h>
#include <lib/zx/result.h>

#include "aml-sdmmc.h"

namespace aml_sdmmc {

// Note: This name can't be changed without migrating users in other repos.
constexpr std::string_view kDriverName = "aml-sd-emmc";

class Dfv2Driver : public fdf::DriverBase, public ddk::SdmmcProtocol<Dfv2Driver>, public AmlSdmmc {
 public:
  Dfv2Driver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase(kDriverName, std::move(start_args), std::move(dispatcher)) {}
  ~Dfv2Driver() override = default;

  void Start(fdf::StartCompleter completer) override;

  // ddk::SdmmcProtocol methods exposed
  zx_status_t SdmmcHostInfo(sdmmc_host_info_t* out_info) { return HostInfo(out_info); }
  zx_status_t SdmmcSetSignalVoltage(sdmmc_voltage_t voltage) { return SetSignalVoltage(voltage); }
  zx_status_t SdmmcSetBusWidth(sdmmc_bus_width_t bus_width) { return SetBusWidth(bus_width); }
  zx_status_t SdmmcSetBusFreq(uint32_t bus_freq) { return SetBusFreq(bus_freq); }
  zx_status_t SdmmcSetTiming(sdmmc_timing_t timing) { return SetTiming(timing); }
  zx_status_t SdmmcHwReset() { return HwReset(); }
  zx_status_t SdmmcPerformTuning(uint32_t cmd_idx) { return PerformTuning(cmd_idx); }
  zx_status_t SdmmcRegisterInBandInterrupt(const in_band_interrupt_protocol_t* interrupt_cb) {
    return RegisterInBandInterrupt(interrupt_cb);
  }
  void SdmmcAckInBandInterrupt() { return AckInBandInterrupt(); }
  zx_status_t SdmmcRegisterVmo(uint32_t vmo_id, uint8_t client_id, zx::vmo vmo, uint64_t offset,
                               uint64_t size, uint32_t vmo_rights) {
    return RegisterVmo(vmo_id, client_id, std::move(vmo), offset, size, vmo_rights);
  }
  zx_status_t SdmmcUnregisterVmo(uint32_t vmo_id, uint8_t client_id, zx::vmo* out_vmo) {
    return UnregisterVmo(vmo_id, client_id, out_vmo);
  }
  zx_status_t SdmmcRequest(const sdmmc_req_t* req, uint32_t out_response[4]) {
    return Request(req, out_response);
  }

 private:
  zx::result<> InitResources(fidl::ClientEnd<fuchsia_hardware_platform_device::Device> pdev_client,
                             aml_sdmmc_config_t config);

  void Serve(fdf::ServerEnd<fuchsia_hardware_sdmmc::Sdmmc> request);

  void CompatServerInitialized(zx::result<> compat_result);
  void CompleteStart(zx::result<> result);

  compat::DeviceServer::BanjoConfig get_banjo_config() {
    compat::DeviceServer::BanjoConfig config{ZX_PROTOCOL_SDMMC};
    config.callbacks[ZX_PROTOCOL_SDMMC] = banjo_server_.callback();
    return config;
  }

  std::optional<inspect::ComponentInspector> exposed_inspector_;

  fidl::WireSyncClient<fuchsia_driver_framework::Node> parent_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;

  std::optional<fdf::StartCompleter> start_completer_;

  compat::BanjoServer banjo_server_{ZX_PROTOCOL_SDMMC, this, &sdmmc_protocol_ops_};
  compat::DeviceServer compat_server_{
      dispatcher(),
      incoming(),
      outgoing(),
      node_name(),
      name(),
      std::nullopt,
      compat::ForwardMetadata::Some({DEVICE_METADATA_SDMMC, DEVICE_METADATA_GPT_INFO}),
      get_banjo_config()};
};

}  // namespace aml_sdmmc

#endif  // SRC_DEVICES_BLOCK_DRIVERS_AML_SDMMC_DFV2_DRIVER_H_
