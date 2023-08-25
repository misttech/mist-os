// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <ddktl/metadata/audio.h>
#include <soc/aml-a311d/a311d-gpio.h>
#include <soc/aml-a311d/a311d-hw.h>
#include <soc/aml-common/aml-audio.h>
#include <soc/aml-meson/g12b-clk.h>

#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"
#include "vim3-gpios.h"
#include "vim3.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace vim3 {
namespace fpbus = fuchsia_hardware_platform_bus;

zx_status_t Vim3::AudioInit() {
  uint8_t tdm_instance_id = 1;
  fidl::Arena<> fidl_arena;
  fdf::Arena fdf_arena('AUDI');
  static const std::vector<fpbus::Mmio> audio_mmios{
      {{
          .base = A311D_EE_AUDIO_BASE,
          .length = A311D_EE_AUDIO_LENGTH,
      }},
  };

  zx_status_t status = clk_impl_.Disable(g12b_clk::CLK_HIFI_PLL);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Disable(CLK_HIFI_PLL) failed: %s", zx_status_get_string(status));
    return status;
  }

  status = clk_impl_.SetRate(g12b_clk::CLK_HIFI_PLL, 768'000'000);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SetRate(CLK_HIFI_PLL) failed: %s", zx_status_get_string(status));
    return status;
  }

  status = clk_impl_.Enable(g12b_clk::CLK_HIFI_PLL);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Enable(CLK_HIFI_PLL) failed: %s", zx_status_get_string(status));
    return status;
  }

  // PCM pin assignments.
  constexpr uint64_t kStrengthUa = 3000;
  // TDM bus A connected to BTPCM.
  gpio_impl_.SetAltFunction(A311D_GPIOX(11), A311D_GPIOX_11_TDMA_SCLK_FN);
  gpio_impl_.SetAltFunction(A311D_GPIOX(10), A311D_GPIOX_10_TDMA_FS_FN);
  gpio_impl_.SetAltFunction(A311D_GPIOX(9), A311D_GPIOX_9_TDMA_D0_FN);
  gpio_impl_.SetAltFunction(A311D_GPIOX(8), A311D_GPIOX_8_TDMA_DIN1_FN);
  gpio_impl_.SetDriveStrength(A311D_GPIOX(11), kStrengthUa, nullptr);
  gpio_impl_.SetDriveStrength(A311D_GPIOX(10), kStrengthUa, nullptr);
  gpio_impl_.SetDriveStrength(A311D_GPIOX(9), kStrengthUa, nullptr);
  // GPIOX(8) is set as input, so no driver strength is set.

  // TDM bus B connected to I2SB.
  gpio_impl_.SetAltFunction(A311D_GPIOA(1), A311D_GPIOA_1_TDMB_SCLK_FN);
  gpio_impl_.SetAltFunction(A311D_GPIOA(2), A311D_GPIOA_2_TDMB_FS_FN);
  gpio_impl_.SetAltFunction(A311D_GPIOA(3), A311D_GPIOA_3_TDMB_D0_FN);
  gpio_impl_.SetAltFunction(A311D_GPIOA(4), A311D_GPIOA_4_TDMB_DIN1_FN);
  gpio_impl_.SetDriveStrength(A311D_GPIOA(1), kStrengthUa, nullptr);
  gpio_impl_.SetDriveStrength(A311D_GPIOA(2), kStrengthUa, nullptr);
  gpio_impl_.SetDriveStrength(A311D_GPIOA(3), kStrengthUa, nullptr);
  // GPIOA(4) is set as input, so no driver strength is set.

  // Output device BTPCM setup with TDM bus A.
  {
    static const std::vector<fpbus::Bti> pcm_out_btis{
        {{
            .iommu_index = 0,
            .bti_id = BTI_AUDIO_BT_OUT,
        }},
    };
    metadata::AmlConfig metadata = {};
    snprintf(metadata.manufacturer, sizeof(metadata.manufacturer), "Spacely Sprockets");
    snprintf(metadata.product_name, sizeof(metadata.product_name), "vim3");

    metadata.is_input = false;
    // Compatible clocks with other TDM drivers.
    metadata.mClockDivFactor = 10;
    metadata.sClockDivFactor = 25;
    metadata.unique_id = AUDIO_STREAM_UNIQUE_ID_BUILTIN_BT;
    metadata.bus = metadata::AmlBus::TDM_A;
    metadata.version = metadata::AmlVersion::kA311D;
    metadata.dai.type = metadata::DaiType::Tdm1;
    metadata.dai.sclk_on_raising = true;
    metadata.dai.bits_per_sample = 16;
    metadata.dai.bits_per_slot = 16;
    metadata.ring_buffer.number_of_channels = 2;
    metadata.dai.number_of_channels = 2;
    metadata.lanes_enable_mask[0] = 3;
    std::vector<fpbus::Metadata> tdm_metadata{
        {{
            .type = DEVICE_METADATA_PRIVATE,
            .data = std::vector<uint8_t>(
                reinterpret_cast<const uint8_t*>(&metadata),
                reinterpret_cast<const uint8_t*>(&metadata) + sizeof(metadata)),
        }},
    };

    auto builder = fuchsia_driver_framework::wire::CompositeNodeSpec::Builder(fdf_arena).name(
        "audio-pcm-out-composite-spec");

    fpbus::Node tdm_dev;
    tdm_dev.name() = "audio-pcm-out";
    tdm_dev.vid() = PDEV_VID_AMLOGIC;
    tdm_dev.pid() = PDEV_PID_AMLOGIC_A311D;
    tdm_dev.did() = PDEV_DID_AMLOGIC_TDM;
    tdm_dev.mmio() = audio_mmios;
    tdm_dev.bti() = pcm_out_btis;
    tdm_dev.metadata() = tdm_metadata;
    tdm_dev.instance_id() = tdm_instance_id++;
    auto result = pbus_.buffer(fdf_arena)->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, tdm_dev),
                                                                builder.Build());
    if (!result.ok()) {
      zxlogf(ERROR, "AddCompositeNodeSpec failed: %s", result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "AddCompositeNodeSpec failed: %s", zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  // Output device I2SB setup with TDM bus B.
  {
    static const std::vector<fpbus::Bti> i2s_out_btis{
        {{
            .iommu_index = 0,
            .bti_id = BTI_AUDIO_OUT,
        }},
    };
    metadata::AmlConfig metadata = {};
    snprintf(metadata.manufacturer, sizeof(metadata.manufacturer), "Spacely Sprockets");
    snprintf(metadata.product_name, sizeof(metadata.product_name), "vim3");

    metadata.is_input = false;
    // Compatible clocks with other TDM drivers.
    metadata.mClockDivFactor = 10;
    metadata.sClockDivFactor = 25;
    metadata.unique_id = AUDIO_STREAM_UNIQUE_ID_BUILTIN_SPEAKERS;  // Through GPIO header.
    metadata.bus = metadata::AmlBus::TDM_B;
    metadata.version = metadata::AmlVersion::kA311D;
    metadata.dai.type = metadata::DaiType::Tdm1;
    metadata.dai.sclk_on_raising = true;
    metadata.dai.bits_per_sample = 16;
    metadata.dai.bits_per_slot = 16;
    metadata.ring_buffer.number_of_channels = 2;
    metadata.dai.number_of_channels = 2;
    metadata.lanes_enable_mask[0] = 3;
    std::vector<fpbus::Metadata> tdm_metadata{
        {{
            .type = DEVICE_METADATA_PRIVATE,
            .data = std::vector<uint8_t>(
                reinterpret_cast<const uint8_t*>(&metadata),
                reinterpret_cast<const uint8_t*>(&metadata) + sizeof(metadata)),
        }},
    };

    auto builder = fuchsia_driver_framework::wire::CompositeNodeSpec::Builder(fdf_arena).name(
        "audio-i2s-out-composite-spec");

    fpbus::Node tdm_dev;
    tdm_dev.name() = "audio-i2s-out";
    tdm_dev.vid() = PDEV_VID_AMLOGIC;
    tdm_dev.pid() = PDEV_PID_AMLOGIC_A311D;
    tdm_dev.did() = PDEV_DID_AMLOGIC_TDM;
    tdm_dev.mmio() = audio_mmios;
    tdm_dev.bti() = i2s_out_btis;
    tdm_dev.metadata() = tdm_metadata;
    tdm_dev.instance_id() = tdm_instance_id++;
    auto result = pbus_.buffer(fdf_arena)->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, tdm_dev),
                                                                builder.Build());
    if (!result.ok()) {
      zxlogf(ERROR, "AddCompositeNodeSpec failed: %s", result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "AddCompositeNodeSpec failed: %s", zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  // Input device BTPCM setup with TDM bus A.
  {
    static const std::vector<fpbus::Bti> pcm_in_btis{
        {{
            .iommu_index = 0,
            .bti_id = BTI_AUDIO_BT_IN,
        }},
    };
    metadata::AmlConfig metadata = {};
    snprintf(metadata.manufacturer, sizeof(metadata.manufacturer), "Spacely Sprockets");
    snprintf(metadata.product_name, sizeof(metadata.product_name), "vim3");
    metadata.is_input = true;
    // Compatible clocks with other TDM drivers.
    metadata.mClockDivFactor = 10;
    metadata.sClockDivFactor = 25;
    metadata.unique_id = AUDIO_STREAM_UNIQUE_ID_BUILTIN_BT;
    metadata.bus = metadata::AmlBus::TDM_A;
    metadata.version = metadata::AmlVersion::kA311D;
    metadata.dai.type = metadata::DaiType::Tdm1;
    metadata.dai.sclk_on_raising = true;
    metadata.dai.bits_per_sample = 16;
    metadata.dai.bits_per_slot = 16;
    metadata.ring_buffer.number_of_channels = 2;
    metadata.dai.number_of_channels = 2;
    metadata.swaps = 0x3200;
    metadata.lanes_enable_mask[1] = 3;
    std::vector<fpbus::Metadata> tdm_metadata{
        {{
            .type = DEVICE_METADATA_PRIVATE,
            .data = std::vector<uint8_t>(
                reinterpret_cast<const uint8_t*>(&metadata),
                reinterpret_cast<const uint8_t*>(&metadata) + sizeof(metadata)),
        }},
    };

    auto builder = fuchsia_driver_framework::wire::CompositeNodeSpec::Builder(fdf_arena).name(
        "audio-pcm-in-composite-spec");

    fpbus::Node tdm_dev;
    tdm_dev.name() = "audio-pcm-in";
    tdm_dev.vid() = PDEV_VID_AMLOGIC;
    tdm_dev.pid() = PDEV_PID_AMLOGIC_A311D;
    tdm_dev.did() = PDEV_DID_AMLOGIC_TDM;
    tdm_dev.mmio() = audio_mmios;
    tdm_dev.bti() = pcm_in_btis;
    tdm_dev.metadata() = tdm_metadata;
    tdm_dev.instance_id() = tdm_instance_id++;

    auto result = pbus_.buffer(fdf_arena)->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, tdm_dev),
                                                                builder.Build());
    if (!result.ok()) {
      zxlogf(ERROR, "AddCompositeNodeSpec failed: %s", result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "AddCompositeNodeSpec failed: %s", zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  // Input device I2SB setup with TDM bus B.
  {
    static const std::vector<fpbus::Bti> i2s_in_btis{
        {{
            .iommu_index = 0,
            .bti_id = BTI_AUDIO_IN,
        }},
    };
    metadata::AmlConfig metadata = {};
    snprintf(metadata.manufacturer, sizeof(metadata.manufacturer), "Spacely Sprockets");
    snprintf(metadata.product_name, sizeof(metadata.product_name), "vim3");
    metadata.is_input = true;
    // Compatible clocks with other TDM drivers.
    metadata.mClockDivFactor = 10;
    metadata.sClockDivFactor = 25;
    metadata.unique_id = AUDIO_STREAM_UNIQUE_ID_BUILTIN_MICROPHONE;  // Through GPIO header.
    metadata.bus = metadata::AmlBus::TDM_B;
    metadata.version = metadata::AmlVersion::kA311D;
    metadata.dai.type = metadata::DaiType::Tdm1;
    metadata.dai.sclk_on_raising = true;
    metadata.dai.bits_per_sample = 16;
    metadata.dai.bits_per_slot = 16;
    metadata.ring_buffer.number_of_channels = 2;
    metadata.dai.number_of_channels = 2;
    metadata.swaps = 0x3200;
    metadata.lanes_enable_mask[1] = 3;
    std::vector<fpbus::Metadata> tdm_metadata{
        {{
            .type = DEVICE_METADATA_PRIVATE,
            .data = std::vector<uint8_t>(
                reinterpret_cast<const uint8_t*>(&metadata),
                reinterpret_cast<const uint8_t*>(&metadata) + sizeof(metadata)),
        }},
    };

    auto builder = fuchsia_driver_framework::wire::CompositeNodeSpec::Builder(fdf_arena).name(
        "audio-i2s-in-composite-spec");

    fpbus::Node tdm_dev;
    tdm_dev.name() = "audio-i2s-in";
    tdm_dev.vid() = PDEV_VID_AMLOGIC;
    tdm_dev.pid() = PDEV_PID_AMLOGIC_A311D;
    tdm_dev.did() = PDEV_DID_AMLOGIC_TDM;
    tdm_dev.mmio() = audio_mmios;
    tdm_dev.bti() = i2s_in_btis;
    tdm_dev.metadata() = tdm_metadata;
    tdm_dev.instance_id() = tdm_instance_id++;

    auto result = pbus_.buffer(fdf_arena)->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, tdm_dev),
                                                                builder.Build());
    if (!result.ok()) {
      zxlogf(ERROR, "AddCompositeNodeSpec failed: %s", result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "AddCompositeNodeSpec failed: %s", zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  return ZX_OK;
}

}  // namespace vim3
