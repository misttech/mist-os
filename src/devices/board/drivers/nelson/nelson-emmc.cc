// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.gpt.metadata/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fuchsia/hardware/sdmmc/c/banjo.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/mmio/mmio.h>
#include <lib/zx/handle.h>
#include <zircon/hw/gpt.h>

#include <ddk/metadata/emmc.h>
#include <soc/aml-common/aml-sdmmc.h>
#include <soc/aml-s905d3/s905d3-gpio.h>
#include <soc/aml-s905d3/s905d3-hw.h>

#include "nelson-gpios.h"
#include "nelson.h"
#include "src/devices/board/drivers/nelson/nelson_emmc_bind.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

namespace nelson {
namespace fpbus = fuchsia_hardware_platform_bus;

namespace {

static const std::vector<fpbus::Mmio> emmc_mmios{
    {{
        .base = S905D3_EMMC_C_SDIO_BASE,
        .length = S905D3_EMMC_C_SDIO_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> emmc_irqs{
    {{
        .irq = S905D3_EMMC_C_SDIO_IRQ,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

static const std::vector<fpbus::Bti> emmc_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_EMMC,
    }},
};

static aml_sdmmc_config_t config = {
    .min_freq = 400'000,
    .max_freq = 166'666'667,
    .version_3 = true,
    .prefs = SDMMC_HOST_PREFS_DISABLE_HS400,
};

const emmc_config_t emmc_config = {
    // Maintain the current Nelson behavior until we determine that trim is needed.
    .enable_trim = true,
    // Maintain the current Nelson behavior until we determine that cache is needed.
    .enable_cache = false,
};

static const struct {
  const fidl::StringView name;
  const fuchsia_hardware_block_partition::wire::Guid guid;
} guid_map[] = {
    {"misc", GUID_ABR_META_VALUE},
    {"boot_a", GUID_ZIRCON_A_VALUE},
    {"boot_b", GUID_ZIRCON_B_VALUE},
    {"cache", GUID_ZIRCON_R_VALUE},
    {"zircon_r", GUID_ZIRCON_R_VALUE},
    {"vbmeta_a", GUID_VBMETA_A_VALUE},
    {"vbmeta_b", GUID_VBMETA_B_VALUE},
    {"vbmeta_r", GUID_VBMETA_R_VALUE},
    {"reserved_c", GUID_VBMETA_R_VALUE},
    {"data", GUID_FVM_VALUE},
    {"fvm", GUID_FVM_VALUE},
};

static_assert(sizeof(guid_map) / sizeof(guid_map[0]) <=
              fuchsia_hardware_gpt_metadata::wire::kMaxPartitions);

static const std::vector<fpbus::BootMetadata> emmc_boot_metadata{
    {{
        .zbi_type = DEVICE_METADATA_PARTITION_MAP,
        .zbi_extra = 0,
    }},
};

}  // namespace

zx_status_t Nelson::EmmcInit() {
  // set alternate functions to enable EMMC
  gpio_impl_.SetAltFunction(S905D3_EMMC_D0, S905D3_EMMC_D0_FN);
  gpio_impl_.SetAltFunction(S905D3_EMMC_D1, S905D3_EMMC_D1_FN);
  gpio_impl_.SetAltFunction(S905D3_EMMC_D2, S905D3_EMMC_D2_FN);
  gpio_impl_.SetAltFunction(S905D3_EMMC_D3, S905D3_EMMC_D3_FN);
  gpio_impl_.SetAltFunction(S905D3_EMMC_D4, S905D3_EMMC_D4_FN);
  gpio_impl_.SetAltFunction(S905D3_EMMC_D5, S905D3_EMMC_D5_FN);
  gpio_impl_.SetAltFunction(S905D3_EMMC_D6, S905D3_EMMC_D6_FN);
  gpio_impl_.SetAltFunction(S905D3_EMMC_D7, S905D3_EMMC_D7_FN);
  gpio_impl_.SetAltFunction(S905D3_EMMC_CLK, S905D3_EMMC_CLK_FN);
  gpio_impl_.SetAltFunction(S905D3_EMMC_RST, S905D3_EMMC_RST_FN);
  gpio_impl_.SetAltFunction(S905D3_EMMC_CMD, S905D3_EMMC_CMD_FN);
  gpio_impl_.SetAltFunction(S905D3_EMMC_DS, S905D3_EMMC_DS_FN);

  fidl::Arena<> fidl_arena;

  fidl::VectorView<fuchsia_hardware_gpt_metadata::wire::PartitionInfo> partition_info(
      fidl_arena, std::size(guid_map));

  for (size_t i = 0; i < std::size(guid_map); i++) {
    partition_info[i].name = guid_map[i].name;
    partition_info[i].options =
        fuchsia_hardware_gpt_metadata::wire::PartitionOptions::Builder(fidl_arena)
            .type_guid_override(guid_map[i].guid)
            .Build();
  }

  fit::result encoded =
      fidl::Persist(fuchsia_hardware_gpt_metadata::wire::GptInfo::Builder(fidl_arena)
                        .partition_info(partition_info)
                        .Build());
  if (!encoded.is_ok()) {
    zxlogf(ERROR, "Failed to encode GPT metadata: %s",
           encoded.error_value().FormatDescription().c_str());
    return encoded.error_value().status();
  }

  static const std::vector<fpbus::Metadata> emmc_metadata{
      {{
          .type = DEVICE_METADATA_PRIVATE,
          .data = std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&config),
                                       reinterpret_cast<const uint8_t*>(&config) + sizeof(config)),
      }},
      {{
          .type = DEVICE_METADATA_GPT_INFO,
          .data = std::move(encoded.value()),
      }},
      {{
          .type = DEVICE_METADATA_EMMC_CONFIG,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&emmc_config),
              reinterpret_cast<const uint8_t*>(&emmc_config) + sizeof(emmc_config)),
      }},
  };

  static const fpbus::Node emmc_dev = []() {
    fpbus::Node dev = {};
    dev.name() = "nelson-emmc";
    dev.vid() = PDEV_VID_AMLOGIC;
    dev.pid() = PDEV_PID_GENERIC;
    dev.did() = PDEV_DID_AMLOGIC_SDMMC_C;
    dev.mmio() = emmc_mmios;
    dev.irq() = emmc_irqs;
    dev.bti() = emmc_btis;
    dev.metadata() = emmc_metadata;
    dev.boot_metadata() = emmc_boot_metadata;
    return dev;
  }();

  fdf::Arena arena('EMMC');
  auto result = pbus_.buffer(arena)->AddComposite(
      fidl::ToWire(fidl_arena, emmc_dev),
      platform_bus_composite::MakeFidlFragment(fidl_arena, nelson_emmc_fragments,
                                               std::size(nelson_emmc_fragments)),
      "pdev");
  if (!result.ok()) {
    zxlogf(ERROR, "%s: AddComposite Emmc(emmc_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: AddComposite Emmc(emmc_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

}  // namespace nelson
