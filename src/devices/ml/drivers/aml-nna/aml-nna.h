// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_ML_DRIVERS_AML_NNA_AML_NNA_H_
#define SRC_DEVICES_ML_DRIVERS_AML_NNA_AML_NNA_H_

#include <fidl/fuchsia.hardware.registers/cpp/wire.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/platform-defs.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/mmio/mmio.h>
#include <zircon/fidl.h>

#include <ddktl/device.h>
#include <ddktl/protocol/empty-protocol.h>
#include <soc/aml-common/aml-power-domain.h>
#include <soc/aml-common/aml-registers.h>

constexpr uint32_t kNnaPowerDomainLegacy = 0;
constexpr uint32_t kNnaPowerDomain = 1;

namespace aml_nna {

class AmlNnaDevice;
using AmlNnaDeviceType = ddk::Device<AmlNnaDevice>;

class AmlNnaDevice : public AmlNnaDeviceType {
 public:
  struct NnaPowerDomainBlock {
    // Power Domain MMIO.
    uint32_t domain_power_sleep_offset;
    uint32_t domain_power_iso_offset;
    // Set power state (1 = power off)
    uint32_t domain_power_sleep_bits;
    // Set control output signal isolation (1 = set isolation)
    uint32_t domain_power_iso_bits;

    // Memory PD MMIO.
    uint32_t hhi_mem_pd_reg0_offset;
    uint32_t hhi_mem_pd_reg1_offset;

    // Reset MMIO.
    uint32_t reset_level2_offset;
  };
  // Each offset is the byte offset of the register in their respective mmio region.
  struct NnaBlock {
    // For the new chips from Amlogic, smc already supports the control of power domain
    // So A5 uses smc to manage the NN power.
    uint32_t nna_power_version;
    union {
      struct NnaPowerDomainBlock nna_regs;  // Access with mmio read/write.
      uint32_t nna_domain_id;               // Access with smc call.
    };
    // Hiu MMIO.
    uint32_t clock_control_offset;
    uint32_t clock_core_control_bits;
    uint32_t clock_axi_control_bits;
  };

  explicit AmlNnaDevice(zx_device_t* parent, fdf::MmioBuffer hiu_mmio, fdf::MmioBuffer power_mmio,
                        fdf::MmioBuffer memory_pd_mmio,
                        fidl::ClientEnd<fuchsia_hardware_registers::Device> reset,
                        ddk::PDevFidl pdev, NnaBlock nna_block, zx::resource smc_monitor)
      : AmlNnaDeviceType(parent),
        pdev_(std::move(pdev)),
        hiu_mmio_(std::move(hiu_mmio)),
        power_mmio_(std::move(power_mmio)),
        memory_pd_mmio_(std::move(memory_pd_mmio)),
        reset_(std::move(reset)),
        nna_block_(nna_block),
        smc_monitor_(std::move(smc_monitor)),
        outgoing_(fdf::Dispatcher::GetCurrent()->async_dispatcher()) {}
  static zx_status_t Create(void* ctx, zx_device_t* parent);

  zx_status_t Init();

  zx_status_t PowerDomainControl(bool turn_on);

  // Methods required by the ddk.
  void DdkRelease();

 private:
  ddk::PDevFidl pdev_;
  fdf::MmioBuffer hiu_mmio_;
  fdf::MmioBuffer power_mmio_;
  fdf::MmioBuffer memory_pd_mmio_;
  fidl::WireSyncClient<fuchsia_hardware_registers::Device> reset_;

  NnaBlock nna_block_;

  // Control PowerDomain
  zx::resource smc_monitor_;
  component::OutgoingDirectory outgoing_;
};

}  // namespace aml_nna

#endif  // SRC_DEVICES_ML_DRIVERS_AML_NNA_AML_NNA_H_
