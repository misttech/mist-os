// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <soc/aml-common/aml-registers.h>

#include "sherlock.h"
#include "src/devices/lib/fidl-metadata/registers.h"

namespace sherlock {
namespace fpbus = fuchsia_hardware_platform_bus;

namespace {

enum MmioMetadataIdx {
  RESET_MMIO,
#ifdef FACTORY_BUILD
  USB_FACTORY_MMIO,
#endif  // FACTORY_BUILD

  MMIO_COUNT,
};

const std::vector<fpbus::Mmio> registers_mmios = {
    {{
        .base = T931_RESET_BASE,
        .length = T931_RESET_LENGTH,
    }},
#ifdef FACTORY_BUILD
    []() {
      fpbus::Mmio ret;
      ret.base() = T931_USB_BASE;
      ret.length() = T931_USB_LENGTH;
      return ret;
    }(),
#endif  // FACTORY_BUILD
};

static const fidl_metadata::registers::Register<uint32_t> kRegisters[]{
    {
        .bind_id = aml_registers::REGISTER_USB_PHY_V2_RESET,
        .mmio_id = RESET_MMIO,
        .masks =
            {
                {
                    .value = aml_registers::USB_RESET1_REGISTER_UNKNOWN_1_MASK |
                             aml_registers::USB_RESET1_REGISTER_UNKNOWN_2_MASK,
                    .mmio_offset = T931_RESET1_REGISTER,
                },
                {
                    .value = aml_registers::USB_RESET1_LEVEL_MASK,
                    .mmio_offset = T931_RESET1_LEVEL,
                },
            },
    },

    {
        .bind_id = aml_registers::REGISTER_NNA_RESET_LEVEL2,
        .mmio_id = RESET_MMIO,
        .masks =
            {
                {
                    .value = aml_registers::NNA_RESET2_LEVEL_MASK,
                    .mmio_offset = T931_RESET2_LEVEL,
                },
            },
    },
    {
        .bind_id = aml_registers::REGISTER_MALI_RESET,
        .mmio_id = RESET_MMIO,
        .masks =
            {
                {
                    .value = aml_registers::MALI_RESET0_MASK,
                    .mmio_offset = T931_RESET0_MASK,
                },
                {
                    .value = aml_registers::MALI_RESET0_MASK,
                    .mmio_offset = T931_RESET0_LEVEL,
                },
                {
                    .value = aml_registers::MALI_RESET2_MASK,
                    .mmio_offset = T931_RESET2_MASK,
                },
                {
                    .value = aml_registers::MALI_RESET2_MASK,
                    .mmio_offset = T931_RESET2_LEVEL,
                },
            },
    },

    {
        .bind_id = aml_registers::REGISTER_ISP_RESET,
        .mmio_id = RESET_MMIO,
        .masks =
            {
                {
                    .value = aml_registers::ISP_RESET4_MASK,
                    .mmio_offset = T931_RESET4_LEVEL,
                },
            },
    },

    {
        .bind_id = aml_registers::REGISTER_SPICC0_RESET,
        .mmio_id = RESET_MMIO,
        .masks =
            {
                {
                    .value = aml_registers::SPICC0_RESET_MASK,
                    .mmio_offset = T931_RESET6_REGISTER,
                },
            },
    },

#ifdef FACTORY_BUILD
    {
        .bind_id = aml_registers::REGISTER_USB_PHY_FACTORY,
        .mmio_id = USB_FACTORY_MMIO,
        .masks =
            {
                {
                    .value = 0xFFFFFFFF,
                    .mmio_offset = 0,
                    .count = T931_USB_LENGTH / sizeof(uint32_t),
                    .overlap_check_on = false,
                },
            },
    },
#endif  // FACTORY_BUILD
};

}  // namespace

zx_status_t Sherlock::RegistersInit() {
  auto metadata_bytes = fidl_metadata::registers::RegistersMetadataToFidl(kRegisters);
  if (!metadata_bytes.is_ok()) {
    zxlogf(ERROR, "%s: Could not build metadata %s\n", __func__, metadata_bytes.status_string());
    return metadata_bytes.error_value();
  }

  std::vector<fpbus::Metadata> registers_metadata{
      {{
          .type = DEVICE_METADATA_REGISTERS,
          .data = metadata_bytes.value(),
      }},
  };

  fpbus::Node registers_dev;
  registers_dev.name() = "registers";
  registers_dev.vid() = PDEV_VID_GENERIC;
  registers_dev.pid() = PDEV_PID_GENERIC;
  registers_dev.did() = PDEV_DID_REGISTERS;
  registers_dev.mmio() = registers_mmios;
  registers_dev.metadata() = registers_metadata;

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('REGI');
  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, registers_dev));
  if (!result.ok()) {
    zxlogf(ERROR, "%s: NodeAdd Registers(registers_dev) request failed: %s", __func__,
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "%s: NodeAdd Registers(registers_dev) failed: %s", __func__,
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

}  // namespace sherlock
