// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_AML_SDMMC_DFV1_DRIVER_H_
#define SRC_DEVICES_BLOCK_DRIVERS_AML_SDMMC_DFV1_DRIVER_H_

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <fuchsia/hardware/sdmmc/cpp/banjo.h>
#include <lib/dma-buffer/buffer.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/mmio/mmio.h>
#include <lib/zx/interrupt.h>

#include <ddktl/device.h>

#include "aml-sdmmc.h"

namespace aml_sdmmc {

class Dfv1Driver;
using Dfv1DriverType = ddk::Device<Dfv1Driver, ddk::Suspendable>;

class Dfv1Driver : public Dfv1DriverType,
                   public ddk::SdmmcProtocol<Dfv1Driver, ddk::base_protocol>,
                   public AmlSdmmc {
 public:
  static zx_status_t Create(void* ctx, zx_device_t* parent);

  explicit Dfv1Driver(zx_device_t* parent)
      : Dfv1DriverType(parent),
        dispatcher_(fdf::Dispatcher::GetCurrent()->get()),
        outgoing_dir_(fdf::OutgoingDirectory::Create(dispatcher_)) {}
  ~Dfv1Driver() override = default;

  // Device protocol implementation
  void DdkRelease();
  void DdkSuspend(ddk::SuspendTxn txn);

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

 protected:
  // Visible for tests
  zx_status_t Bind();

 private:
  // Dispatcher for being a FIDL server serving Sdmmc requests.
  fdf_dispatcher_t* dispatcher_;
  // Serves fuchsia_hardware_sdmmc::SdmmcService.
  fdf::OutgoingDirectory outgoing_dir_;
};

}  // namespace aml_sdmmc

#endif  // SRC_DEVICES_BLOCK_DRIVERS_AML_SDMMC_DFV1_DRIVER_H_
