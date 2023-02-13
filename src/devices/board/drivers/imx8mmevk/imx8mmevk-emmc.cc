// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.nxp.sdmmc/cpp/wire.h>
#include <fuchsia/hardware/sdmmc/cpp/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/nxp/platform/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>

#include "imx8mmevk.h"
#include "src/devices/lib/nxp/include/soc/imx8mm/usdhc.h"

namespace imx8mm_evk {
namespace fpbus = fuchsia_hardware_platform_bus;

zx_status_t Imx8mmEvk::EmmcInit() {
  static const std::vector<fpbus::Mmio> emmc_mmios{
      {{
          .base = imx8mm::kUsdhc3Base,
          .length = imx8mm::kUsdhcSize,
      }},
  };

  static const std::vector<fpbus::Irq> emmc_irqs{
      {{
          .irq = imx8mm::kUsdhc3Irq,
          .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
      }},
  };

  static const std::vector<fpbus::Bti> emmc_btis{
      {{
          .iommu_index = 0,
          .bti_id = BTI_EMMC,
      }},
  };

  fuchsia_nxp_sdmmc::wire::SdmmcMetadata metadata = {
      .tuning_start_tap = 20, .tuning_step = 2, .bus_width = 8};
  fit::result encoded = fidl::Persist(metadata);
  if (!encoded.is_ok()) {
    zxlogf(ERROR, "Failed to encode sdmmc metadata: %s",
           encoded.error_value().FormatDescription().c_str());
    return encoded.error_value().status();
  }

  static const std::vector<fpbus::Metadata> emmc_metadata{
      {{
          .type = DEVICE_METADATA_PRIVATE,
          .data = std::move(encoded.value()),
      }},
  };

  static const std::vector<fpbus::BootMetadata> emmc_boot_metadata{
      {{
          .zbi_type = DEVICE_METADATA_PARTITION_MAP,
          .zbi_extra = 0,
      }},
  };

  fpbus::Node emmc_dev;
  emmc_dev.name() = "imx8m_emmc";
  emmc_dev.vid() = PDEV_VID_NXP;
  emmc_dev.pid() = PDEV_PID_IMX8MMEVK;
  emmc_dev.did() = PDEV_DID_IMX_SDHCI;
  emmc_dev.mmio() = emmc_mmios;
  emmc_dev.irq() = emmc_irqs;
  emmc_dev.bti() = emmc_btis;
  emmc_dev.metadata() = emmc_metadata;
  emmc_dev.boot_metadata() = emmc_boot_metadata;
  emmc_dev.instance_id() = 0;

  const ddk::BindRule kPdevRules[] = {
      ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL, bind_fuchsia_platform::BIND_PROTOCOL_DEVICE),
      ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_VID,
                              bind_fuchsia_nxp_platform::BIND_PLATFORM_DEV_VID_NXP),
      ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_PID,
                              bind_fuchsia_nxp_platform::BIND_PLATFORM_DEV_PID_IMX8MMEVK),
      ddk::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_DID,
                              bind_fuchsia_nxp_platform::BIND_PLATFORM_DEV_DID_SDHCI),
  };

  const device_bind_prop_t kPdevProperties[] = {
      ddk::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_platform::BIND_PROTOCOL_DEVICE),
      ddk::MakeProperty(bind_fuchsia::PLATFORM_DEV_VID,
                        bind_fuchsia_nxp_platform::BIND_PLATFORM_DEV_VID_NXP),
      ddk::MakeProperty(bind_fuchsia::PLATFORM_DEV_DID,
                        bind_fuchsia_nxp_platform::BIND_PLATFORM_DEV_DID_SDHCI),
  };

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('EMMC');

  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, emmc_dev));
  if (!result.ok()) {
    zxlogf(ERROR, "NodeAdd emmc_dev request failed: %s", result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "NodeAdd emmc_dev failed: %s", zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  // TODO(fxbug.dev/121201): Add clock fragment, and replace the PlatformBus::NodeAdd()
  // and DdkAddCompositeNodeSpec() calls with PlatformBus::AddNodeGroup().
  auto status =
      DdkAddCompositeNodeSpec("imx8m_emmc", ddk::CompositeNodeSpec(kPdevRules, kPdevProperties));
  if (status != ZX_OK) {
    zxlogf(INFO, "DdkAddCompositeNodeSpec failed: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

}  // namespace imx8mm_evk
