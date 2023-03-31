// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/io-buffer.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <unistd.h>
#include <zircon/hw/gpt.h>

#include <ddk/metadata/nand.h>
#include <soc/aml-a1/a1-hw.h>
#include <soc/aml-common/aml-guid.h>

#include "clover.h"

namespace clover {
namespace fpbus = fuchsia_hardware_platform_bus;

static const std::vector<fpbus::Mmio> spi_nand_mmios{
    {{
        .base = A1_SPIFC_BASE,
        .length = A1_SPIFC_LENGTH,
    }},
    {{
        .base = A1_SPIFC_CLOCK_BASE,
        .length = A1_SPIFC_CLOCK_LENGTH,
    }},
};

static const nand_config_t config = {
    .bad_block_config =
        {
            .type = kAmlogicUboot,
            .aml_uboot =
                {
                    .table_start_block = 20,
                    .table_end_block = 23,
                },
        },
    .extra_partition_config_count = 2,
    .extra_partition_config =
        {
            {
                .type_guid = GUID_BL2_VALUE,
                .copy_count = 8,
                .copy_byte_offset = 0,
            },
            {
                .type_guid = GUID_BOOTLOADER_VALUE,
                .copy_count = 4,
                .copy_byte_offset = 0,
            },
        },
};

static const std::vector<fpbus::Metadata> spi_nand_metadata{
    {{
        .type = DEVICE_METADATA_PRIVATE,
        .data = std::vector<uint8_t>(reinterpret_cast<const uint8_t*>(&config),
                                     reinterpret_cast<const uint8_t*>(&config) + sizeof(config)),
    }},
};

static const std::vector<fpbus::BootMetadata> spi_nand_boot_metadata{
    {{
        .zbi_type = DEVICE_METADATA_PARTITION_MAP,
        .zbi_extra = 0,
    }},
};

static const fpbus::Node spi_nand_dev = []() {
  fpbus::Node dev = {};
  dev.name() = "spi_nand";
  dev.vid() = PDEV_VID_AMLOGIC;
  dev.pid() = PDEV_PID_GENERIC;
  dev.did() = PDEV_DID_AMLOGIC_SPI_NAND;
  dev.mmio() = spi_nand_mmios;
  dev.metadata() = spi_nand_metadata;
  dev.boot_metadata() = spi_nand_boot_metadata;
  return dev;
}();

zx_status_t Clover::SpiNandInit() {
  fidl::Arena<> fidl_arena;
  fdf::Arena arena('SPIN');
  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, spi_nand_dev));
  if (!result.ok()) {
    zxlogf(ERROR, "NodeAdd SpiNand(spi_nand_dev) request failed: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "NodeAdd SpiNand(spi_nand_dev) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

}  // namespace clover
