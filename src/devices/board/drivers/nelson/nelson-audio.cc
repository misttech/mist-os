// Copyright 2018 The Fuchsia Authors. All rights reserved.
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
#include <bind/fuchsia/codec/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/i2c/cpp/bind.h>
#include <bind/fuchsia/ti/platform/cpp/bind.h>
#include <ddktl/metadata/audio.h>
#include <soc/aml-common/aml-audio.h>
#include <soc/aml-meson/sm1-clk.h>
#include <soc/aml-s905d3/s905d3-gpio.h>
#include <soc/aml-s905d3/s905d3-hw.h>
#include <ti/ti-audio.h>

#include "nelson-gpios.h"
#include "nelson.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

#ifdef TAS5805M_CONFIG_PATH
#include TAS5805M_CONFIG_PATH
#endif

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

// Enables BT PCM audio.
#define ENABLE_BT

namespace nelson {
namespace fpbus = fuchsia_hardware_platform_bus;

// Audio out controller composite node specifications.
const std::vector<fdf::BindRule> kAudioEnableGpioRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_hardware_gpio::BIND_FIDL_PROTOCOL_SERVICE),
    fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(GPIO_SOC_AUDIO_EN)),
};
const std::vector<fdf::NodeProperty> kAudioEnableGpioProps = std::vector{
    fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                      bind_fuchsia_hardware_gpio::BIND_FIDL_PROTOCOL_SERVICE),
    fdf::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                      bind_fuchsia_hardware_gpio::FUNCTION_SOC_AUDIO_ENABLE),
};

const std::vector<fdf::BindRule> kOutCodecRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_codec::BIND_FIDL_PROTOCOL_SERVICE),
    fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_VID,
                            bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_VID_TI),
    fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_DID,
                            bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_DID_TAS58XX),
};
const std::vector<fdf::NodeProperty> kOutCodecProps = std::vector{
    fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_codec::BIND_FIDL_PROTOCOL_SERVICE),
    fdf::MakeProperty(bind_fuchsia::CODEC_INSTANCE, static_cast<uint32_t>(1)),
};

const std::vector<fdf::ParentSpec> kOutControllerParents = std::vector{
    fdf::ParentSpec{{kAudioEnableGpioRules, kAudioEnableGpioProps}},
    fdf::ParentSpec{{kOutCodecRules, kOutCodecProps}},
};

// Codec composite node specifications.
const ddk::BindRule kOutI2cRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
    ddk::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, static_cast<uint32_t>(NELSON_I2C_3)),
    ddk::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS, static_cast<uint32_t>(I2C_AUDIO_CODEC_ADDR)),
};
const device_bind_prop_t kOutI2cProps[] = {
    ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
    ddk::MakeProperty(bind_fuchsia::PLATFORM_DEV_VID,
                      bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_VID_TI),
    ddk::MakeProperty(bind_fuchsia::PLATFORM_DEV_DID,
                      bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_DID_TAS58XX),
};

const ddk::BindRule kFaultGpioRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::PROTOCOL,
                            bind_fuchsia_hardware_gpio::BIND_PROTOCOL_DEVICE),
    ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(GPIO_AUDIO_SOC_FAULT_L)),
};
const device_bind_prop_t kFaultGpioProps[] = {
    ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                      bind_fuchsia_hardware_gpio::BIND_FIDL_PROTOCOL_SERVICE),
    ddk::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                      bind_fuchsia_hardware_gpio::FUNCTION_SOC_AUDIO_FAULT),
};

zx_status_t Nelson::AudioInit() {
  zx_status_t status;

  status = clk_impl_.Disable(sm1_clk::CLK_HIFI_PLL);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Disable(CLK_HIFI_PLL) failed: %s", zx_status_get_string(status));
    return status;
  }

  status = clk_impl_.SetRate(sm1_clk::CLK_HIFI_PLL, 768000000);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SetRate(CLK_HIFI_PLL) failed: %s", zx_status_get_string(status));
    return status;
  }

  status = clk_impl_.Enable(sm1_clk::CLK_HIFI_PLL);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Enable(CLK_HIFI_PLL) failed: %s", zx_status_get_string(status));
    return status;
  }

  static const std::vector<fpbus::Mmio> audio_mmios{
      {{
          .base = S905D3_EE_AUDIO_BASE,
          .length = S905D3_EE_AUDIO_LENGTH,
      }},
  };

  static const std::vector<fpbus::Bti> btis_out{
      {{
          .iommu_index = 0,
          .bti_id = BTI_AUDIO_OUT,
      }},
  };

  static const std::vector<fpbus::Irq> frddr_b_irqs{
      {{
          .irq = S905D3_AUDIO_FRDDR_B,
          .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
      }},
  };
  static const std::vector<fpbus::Irq> toddr_b_irqs{
      {{
          .irq = S905D3_AUDIO_TODDR_B,
          .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
      }},
  };

  static const std::vector<fpbus::Mmio> pdm_mmios{
      {{
          .base = S905D3_EE_PDM_BASE,
          .length = S905D3_EE_PDM_LENGTH,
      }},
      {{
          .base = S905D3_EE_AUDIO_BASE,
          .length = S905D3_EE_AUDIO_LENGTH,
      }},
  };

  static const std::vector<fpbus::Bti> btis_in{
      {{
          .iommu_index = 0,
          .bti_id = BTI_AUDIO_IN,
      }},
  };

  // TDM pin assignments.
  gpio_impl_.SetAltFunction(GPIO_SOC_I2S_SCLK, S905D3_GPIOA_1_TDMB_SCLK_FN);
  gpio_impl_.SetAltFunction(GPIO_SOC_I2S_FS, S905D3_GPIOA_2_TDMB_FS_FN);
  gpio_impl_.SetAltFunction(GPIO_SOC_I2S_DO0, S905D3_GPIOA_3_TDMB_D0_FN);
  constexpr uint64_t ua = 3000;
  gpio_impl_.SetDriveStrength(GPIO_SOC_I2S_SCLK, ua, nullptr);
  gpio_impl_.SetDriveStrength(GPIO_SOC_I2S_FS, ua, nullptr);
  gpio_impl_.SetDriveStrength(GPIO_SOC_I2S_DO0, ua, nullptr);

#ifdef ENABLE_BT
  // PCM pin assignments.
  gpio_impl_.SetAltFunction(GPIO_SOC_BT_PCM_IN, S905D3_GPIOX_8_TDMA_DIN1_FN);
  gpio_impl_.SetAltFunction(GPIO_SOC_BT_PCM_OUT, S905D3_GPIOX_9_TDMA_D0_FN);
  gpio_impl_.SetAltFunction(GPIO_SOC_BT_PCM_SYNC, S905D3_GPIOX_10_TDMA_FS_FN);
  gpio_impl_.SetAltFunction(GPIO_SOC_BT_PCM_CLK, S905D3_GPIOX_11_TDMA_SCLK_FN);
  gpio_impl_.SetDriveStrength(GPIO_SOC_BT_PCM_OUT, ua, nullptr);
  gpio_impl_.SetDriveStrength(GPIO_SOC_BT_PCM_SYNC, ua, nullptr);
  gpio_impl_.SetDriveStrength(GPIO_SOC_BT_PCM_CLK, ua, nullptr);
#endif

  // PDM pin assignments
  gpio_impl_.SetAltFunction(GPIO_SOC_MIC_DCLK, S905D3_GPIOA_7_PDM_DCLK_FN);
  gpio_impl_.SetAltFunction(GPIO_SOC_MICLR_DIN0, S905D3_GPIOA_8_PDM_DIN0_FN);  // First 2 MICs.
  gpio_impl_.SetAltFunction(GPIO_SOC_MICLR_DIN1, S905D3_GPIOA_9_PDM_DIN1_FN);  // Third MIC.

  // Board info.
  fidl::Arena<> fidl_arena;
  fdf::Arena arena('AUDI');
  auto result = pbus_.buffer(arena)->GetBoardInfo();
  if (!result.ok()) {
    zxlogf(ERROR, "NodeAdd Audio(dev_in) request failed: %s", result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "NodeAdd Audio(dev_in) failed: %s", zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  auto board_info = fidl::ToNatural(result->value()->info);

  // Output devices.
  metadata::AmlConfig aml_metadata = {};
  snprintf(aml_metadata.manufacturer, sizeof(aml_metadata.manufacturer), "Spacely Sprockets");
  snprintf(aml_metadata.product_name, sizeof(aml_metadata.product_name), "nelson");
  aml_metadata.is_input = false;
  aml_metadata.mClockDivFactor = 10;
  aml_metadata.sClockDivFactor = 25;
  aml_metadata.unique_id = AUDIO_STREAM_UNIQUE_ID_BUILTIN_SPEAKERS;
  aml_metadata.bus = metadata::AmlBus::TDM_B;
  aml_metadata.version = metadata::AmlVersion::kS905D3G;
  aml_metadata.dai.type = metadata::DaiType::I2s;
  aml_metadata.dai.bits_per_sample = 16;
  aml_metadata.dai.bits_per_slot = 32;

  // We expose a mono ring buffer to clients. However we still use a 2 channels DAI to the codec
  // so we configure the audio engine to only take the one channel and put it in the left slot
  // going out to the codec via I2S.
  aml_metadata.ring_buffer.number_of_channels = 1;
  aml_metadata.swaps = 0x10;              // One ring buffer channel goes into the left I2S slot.
  aml_metadata.lanes_enable_mask[0] = 2;  // One ring buffer channel goes into the left I2S slot.
  aml_metadata.codecs.number_of_codecs = 1;
  aml_metadata.codecs.types[0] = metadata::CodecType::Tas58xx;
  aml_metadata.codecs.channels_to_use_bitmask[0] = 1;  // Codec must use the left I2S slot.
  aml_metadata.codecs.ring_buffer_channels_to_use_bitmask[0] = 0x1;  // Single speaker uses index 0.

  std::vector<fpbus::Metadata> tdm_metadata{
      {{
          .type = DEVICE_METADATA_PRIVATE,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&aml_metadata),
              reinterpret_cast<const uint8_t*>(&aml_metadata) + sizeof(aml_metadata)),
      }},
  };

  fpbus::Node controller_out;
  controller_out.name() = "nelson-audio-i2s-out";
  controller_out.vid() = PDEV_VID_AMLOGIC;
  controller_out.pid() = PDEV_PID_AMLOGIC_S905D3;
  controller_out.did() = PDEV_DID_AMLOGIC_TDM;
  controller_out.mmio() = audio_mmios;
  controller_out.bti() = btis_out;
  controller_out.irq() = frddr_b_irqs;
  controller_out.metadata() = tdm_metadata;

  // CODEC pin assignments.
  gpio_impl_.SetAltFunction(GPIO_INRUSH_EN_SOC, 0);  // BOOST_EN_SOC as GPIO.
  gpio_impl_.ConfigOut(GPIO_INRUSH_EN_SOC, 1);       // BOOST_EN_SOC to high.
  // From the TAS5805m codec reference manual:
  // "9.5.3.1 Startup Procedures
  // 1. Configure ADR/FAULT pin with proper settings for I2C device address.
  // 2. Bring up power supplies (it does not matter if PVDD or DVDD comes up first).
  // 3. Once power supplies are stable, bring up PDN to High and wait 5ms at least, then
  // start SCLK, LRCLK.
  // 4. Once I2S clocks are stable, set the device into HiZ state and enable DSP via the I2C
  // control port.
  // 5. Wait 5ms at least. Then initialize the DSP Coefficient, then set the device to Play
  // state.
  // 6. The device is now in normal operation."
  // Step 3 PDN setup and 5ms delay is executed below.
  gpio_impl_.ConfigOut(GPIO_SOC_AUDIO_EN, 1);  // Set PDN_N to high.
  zx_nanosleep(zx_deadline_after(ZX_MSEC(5)));
  // I2S clocks are configured by the controller and the rest of the initialization is done
  // in the codec itself.
  metadata::ti::TasConfig tas_metadata = {};
  tas_metadata.bridged = true;
#ifdef TAS5805M_CONFIG_PATH
  tas_metadata.number_of_writes1 = sizeof(tas5805m_init_sequence1) / sizeof(cfg_reg);
  for (size_t i = 0; i < tas_metadata.number_of_writes1; ++i) {
    tas_metadata.init_sequence1[i].address = tas5805m_init_sequence1[i].offset;
    tas_metadata.init_sequence1[i].value = tas5805m_init_sequence1[i].value;
  }
  tas_metadata.number_of_writes2 = sizeof(tas5805m_init_sequence2) / sizeof(cfg_reg);
  for (size_t i = 0; i < tas_metadata.number_of_writes2; ++i) {
    tas_metadata.init_sequence2[i].address = tas5805m_init_sequence2[i].offset;
    tas_metadata.init_sequence2[i].value = tas5805m_init_sequence2[i].value;
  }
#endif
  const device_metadata_t codec_metadata[] = {
      {
          .type = DEVICE_METADATA_PRIVATE,
          .data = reinterpret_cast<uint8_t*>(&tas_metadata),
          .length = sizeof(tas_metadata),
      },
  };
  status = DdkAddCompositeNodeSpec("tas58xx", ddk::CompositeNodeSpec(kOutI2cRules, kOutI2cProps)
                                                  .AddParentSpec(kFaultGpioRules, kFaultGpioProps)
                                                  .set_metadata(codec_metadata));
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s DdkAddCompositeNodeSpec failed %d", __FILE__, status);
    return status;
  }
  {
    auto controller_out_spec = fdf::CompositeNodeSpec{{
        "aml_tdm",
        kOutControllerParents,
    }};
    auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(
        fidl::ToWire(fidl_arena, controller_out), fidl::ToWire(fidl_arena, controller_out_spec));
    if (!result.ok()) {
      zxlogf(ERROR, "AddCompositeNodeSpec Audio(controller_out) request failed: %s",
             result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "AddCompositeNodeSpec Audio(controller_out) failed: %s",
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

#ifdef ENABLE_BT
  // Add TDM OUT for BT.
  {
    static const std::vector<fpbus::Bti> pcm_out_btis{
        {{
            .iommu_index = 0,
            .bti_id = BTI_AUDIO_BT_OUT,
        }},
    };
    metadata::AmlConfig metadata = {};
    snprintf(metadata.manufacturer, sizeof(metadata.manufacturer), "Spacely Sprockets");
    snprintf(metadata.product_name, sizeof(metadata.product_name), "nelson");

    metadata.is_input = false;
    // Compatible clocks with other TDM drivers.
    metadata.mClockDivFactor = 10;
    metadata.sClockDivFactor = 25;
    metadata.unique_id = AUDIO_STREAM_UNIQUE_ID_BUILTIN_BT;
    metadata.bus = metadata::AmlBus::TDM_A;
    metadata.version = metadata::AmlVersion::kS905D3G;
    metadata.dai.type = metadata::DaiType::Tdm1;
    metadata.dai.sclk_on_raising = true;
    metadata.dai.bits_per_sample = 16;
    metadata.dai.bits_per_slot = 16;
    metadata.ring_buffer.number_of_channels = 1;
    metadata.dai.number_of_channels = 1;
    metadata.lanes_enable_mask[0] = 1;
    std::vector<fpbus::Metadata> tdm_metadata{
        {{
            .type = DEVICE_METADATA_PRIVATE,
            .data = std::vector<uint8_t>(
                reinterpret_cast<const uint8_t*>(&metadata),
                reinterpret_cast<const uint8_t*>(&metadata) + sizeof(metadata)),
        }},
    };

    fpbus::Node tdm_dev;
    tdm_dev.name() = "nelson-pcm-dai-out";
    tdm_dev.vid() = PDEV_VID_AMLOGIC;
    tdm_dev.pid() = PDEV_PID_AMLOGIC_S905D3;
    tdm_dev.did() = PDEV_DID_AMLOGIC_DAI_OUT;
    tdm_dev.mmio() = audio_mmios;
    tdm_dev.bti() = pcm_out_btis;
    tdm_dev.metadata() = tdm_metadata;
    auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, tdm_dev));
    if (!result.ok()) {
      zxlogf(ERROR, "NodeAdd request failed: %s", result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "NodeAdd failed: %s", zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }
#endif

  // Input devices.
#ifdef ENABLE_BT
  // Add TDM IN for BT.
  {
    static const std::vector<fpbus::Bti> pcm_in_btis{
        {{
            .iommu_index = 0,
            .bti_id = BTI_AUDIO_BT_IN,
        }},
    };
    metadata::AmlConfig metadata = {};
    snprintf(metadata.manufacturer, sizeof(metadata.manufacturer), "Spacely Sprockets");
    snprintf(metadata.product_name, sizeof(metadata.product_name), "nelson");
    metadata.is_input = true;
    // Compatible clocks with other TDM drivers.
    metadata.mClockDivFactor = 10;
    metadata.sClockDivFactor = 25;
    metadata.unique_id = AUDIO_STREAM_UNIQUE_ID_BUILTIN_BT;
    metadata.bus = metadata::AmlBus::TDM_A;
    metadata.version = metadata::AmlVersion::kS905D3G;
    metadata.dai.type = metadata::DaiType::Tdm1;
    metadata.dai.sclk_on_raising = true;
    metadata.dai.bits_per_sample = 16;
    metadata.dai.bits_per_slot = 16;
    metadata.ring_buffer.number_of_channels = 1;
    metadata.dai.number_of_channels = 1;
    metadata.swaps = 0x0200;
    metadata.lanes_enable_mask[1] = 1;
    std::vector<fpbus::Metadata> tdm_metadata{
        {{
            .type = DEVICE_METADATA_PRIVATE,
            .data = std::vector<uint8_t>(
                reinterpret_cast<const uint8_t*>(&metadata),
                reinterpret_cast<const uint8_t*>(&metadata) + sizeof(metadata)),
        }},
    };
    fpbus::Node tdm_dev;
    tdm_dev.name() = "nelson-pcm-dai-in";
    tdm_dev.vid() = PDEV_VID_AMLOGIC;
    tdm_dev.pid() = PDEV_PID_AMLOGIC_S905D3;
    tdm_dev.did() = PDEV_DID_AMLOGIC_DAI_IN;
    tdm_dev.mmio() = audio_mmios;
    tdm_dev.bti() = pcm_in_btis;
    tdm_dev.metadata() = tdm_metadata;

    auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, tdm_dev));
    if (!result.ok()) {
      zxlogf(ERROR, "NodeAdd request failed: %s", result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "NodeAdd failed: %s", zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }
#endif

  // PDM.
  {
    metadata::AmlPdmConfig metadata = {};
    snprintf(metadata.manufacturer, sizeof(metadata.manufacturer), "Spacely Sprockets");
    snprintf(metadata.product_name, sizeof(metadata.product_name), "nelson");
    metadata.number_of_channels = 3;
    metadata.version = metadata::AmlVersion::kS905D3G;
    metadata.sysClockDivFactor = 4;
    metadata.dClockDivFactor = 250;
    std::vector<fpbus::Metadata> pdm_metadata{
        {{
            .type = DEVICE_METADATA_PRIVATE,
            .data = std::vector<uint8_t>(
                reinterpret_cast<const uint8_t*>(&metadata),
                reinterpret_cast<const uint8_t*>(&metadata) + sizeof(metadata)),
        }},
    };

    fpbus::Node dev_in;
    dev_in.name() = "nelson-audio-pdm-in";
    dev_in.vid() = PDEV_VID_AMLOGIC;
    dev_in.pid() = PDEV_PID_AMLOGIC_S905D3;
    dev_in.did() = PDEV_DID_AMLOGIC_PDM;
    dev_in.mmio() = pdm_mmios;
    dev_in.bti() = btis_in;
    dev_in.irq() = toddr_b_irqs;
    dev_in.metadata() = pdm_metadata;

    auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, dev_in));
    if (!result.ok()) {
      zxlogf(ERROR, "NodeAdd Audio(dev_in) request failed: %s", result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "NodeAdd Audio(dev_in) failed: %s",
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }
  return ZX_OK;
}

}  // namespace nelson
