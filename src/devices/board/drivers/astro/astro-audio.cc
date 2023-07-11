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
#include <soc/aml-meson/g12a-clk.h>
#include <soc/aml-s905d2/s905d2-gpio.h>
#include <soc/aml-s905d2/s905d2-hw.h>
#include <ti/ti-audio.h>

#include "astro-gpios.h"
#include "astro.h"
#include "src/devices/bus/lib/platform-bus-composites/platform-bus-composite.h"

// Enables BT PCM audio.
#define ENABLE_BT
// Enable DAI mode for BT PCM audio.
#define ENABLE_DAI_MODE

#ifdef ENABLE_BT
#ifndef ENABLE_DAI_MODE
static const device_fragment_t tdm_pcm_fragments[] = {};
#endif
#endif

#ifdef TAS2770_CONFIG_PATH
#include TAS2770_CONFIG_PATH
#endif

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace astro {
namespace fpbus = fuchsia_hardware_platform_bus;

constexpr uint32_t kCodecVid = bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_VID_TI;
constexpr uint32_t kCodecDid = bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_DID_TAS2770;

static const std::vector<fpbus::Mmio> audio_mmios{
    {{
        .base = S905D2_EE_AUDIO_BASE,
        .length = S905D2_EE_AUDIO_LENGTH,
    }},
};

static const std::vector<fpbus::Irq> frddr_b_irqs{
    {{
        .irq = S905D2_AUDIO_FRDDR_B,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};
static const std::vector<fpbus::Irq> toddr_b_irqs{
    {{
        .irq = S905D2_AUDIO_TODDR_B,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};

#ifdef ENABLE_BT
#ifndef ENABLE_DAI_MODE
static const std::vector<fpbus::Irq> frddr_a_irqs{
    {{
        .irq = S905D2_AUDIO_FRDDR_A,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};
static const std::vector<fpbus::Irq> toddr_a_irqs{
    {{
        .irq = S905D2_AUDIO_TODDR_A,
        .mode = ZX_INTERRUPT_MODE_EDGE_HIGH,
    }},
};
#endif
#endif

static const std::vector<fpbus::Bti> tdm_btis{
    {{
        .iommu_index = 0,
        .bti_id = BTI_AUDIO_OUT,
    }},
};

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

const std::vector<fdf::BindRule> kCodecRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_codec::BIND_FIDL_PROTOCOL_SERVICE),
    fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_VID, kCodecVid),
    fdf::MakeAcceptBindRule(bind_fuchsia::PLATFORM_DEV_DID, kCodecDid),
};
const std::vector<fdf::NodeProperty> kCodecProps = std::vector{
    fdf::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_codec::BIND_FIDL_PROTOCOL_SERVICE),
    fdf::MakeProperty(bind_fuchsia::CODEC_INSTANCE, static_cast<uint32_t>(1)),
};

const ddk::BindRule kI2cRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
    ddk::MakeAcceptBindRule(bind_fuchsia::I2C_BUS_ID, static_cast<uint32_t>(ASTRO_I2C_3)),
    ddk::MakeAcceptBindRule(bind_fuchsia::I2C_ADDRESS,
                            bind_fuchsia_i2c::BIND_I2C_ADDRESS_AUDIO_CODEC),
};
const device_bind_prop_t kI2cProps[] = {
    ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL, bind_fuchsia_i2c::BIND_FIDL_PROTOCOL_DEVICE),
    ddk::MakeProperty(bind_fuchsia::I2C_ADDRESS, bind_fuchsia_i2c::BIND_I2C_ADDRESS_AUDIO_CODEC),
    ddk::MakeProperty(bind_fuchsia::PLATFORM_DEV_VID,
                      bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_VID_TI),
    ddk::MakeProperty(bind_fuchsia::PLATFORM_DEV_DID,
                      bind_fuchsia_ti_platform::BIND_PLATFORM_DEV_DID_TAS2770),
};

const ddk::BindRule kFaultGpioRules[] = {
    ddk::MakeAcceptBindRule(bind_fuchsia::FIDL_PROTOCOL,
                            bind_fuchsia_hardware_gpio::BIND_FIDL_PROTOCOL_SERVICE),
    ddk::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(GPIO_AUDIO_SOC_FAULT_L)),
};
const device_bind_prop_t kFaultGpioProps[] = {
    ddk::MakeProperty(bind_fuchsia::FIDL_PROTOCOL,
                      bind_fuchsia_hardware_gpio::BIND_FIDL_PROTOCOL_SERVICE),
    ddk::MakeProperty(bind_fuchsia_hardware_gpio::FUNCTION,
                      bind_fuchsia_hardware_gpio::FUNCTION_SOC_AUDIO_FAULT),
};

const std::vector<fdf::ParentSpec> kTdmI2sSpec = std::vector{
    fdf::ParentSpec{{kAudioEnableGpioRules, kAudioEnableGpioProps}},
    fdf::ParentSpec{{kCodecRules, kCodecProps}},
};

zx_status_t Astro::AudioInit() {
  zx_status_t status;
  fidl::Arena<> fidl_arena;
  fdf::Arena arena('AUDI');
  uint8_t tdm_instance_id = 1;

  status = clk_impl_.Disable(g12a_clk::CLK_HIFI_PLL);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: Disable(CLK_HIFI_PLL) failed, st = %d", __func__, status);
    return status;
  }

  status = clk_impl_.SetRate(g12a_clk::CLK_HIFI_PLL, 768000000);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: SetRate(CLK_HIFI_PLL) failed, st = %d", __func__, status);
    return status;
  }

  status = clk_impl_.Enable(g12a_clk::CLK_HIFI_PLL);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: Enable(CLK_HIFI_PLL) failed, st = %d", __func__, status);
    return status;
  }

  // TDM pin assignments
  gpio_impl_.SetAltFunction(S905D2_GPIOA(1), S905D2_GPIOA_1_TDMB_SCLK_FN);
  gpio_impl_.SetAltFunction(S905D2_GPIOA(2), S905D2_GPIOA_2_TDMB_FS_FN);
  gpio_impl_.SetAltFunction(S905D2_GPIOA(3), S905D2_GPIOA_3_TDMB_D0_FN);
  gpio_impl_.SetAltFunction(S905D2_GPIOA(6), S905D2_GPIOA_6_TDMB_DIN3_FN);
  constexpr uint64_t ua = 3000;
  gpio_impl_.SetDriveStrength(S905D2_GPIOA(1), ua, nullptr);
  gpio_impl_.SetDriveStrength(S905D2_GPIOA(2), ua, nullptr);
  gpio_impl_.SetDriveStrength(S905D2_GPIOA(3), ua, nullptr);

#ifdef ENABLE_BT
  // PCM pin assignments.
  gpio_impl_.SetAltFunction(S905D2_GPIOX(8), S905D2_GPIOX_8_TDMA_DIN1_FN);
  gpio_impl_.SetAltFunction(S905D2_GPIOX(9), S905D2_GPIOX_9_TDMA_D0_FN);
  gpio_impl_.SetAltFunction(S905D2_GPIOX(10), S905D2_GPIOX_10_TDMA_FS_FN);
  gpio_impl_.SetAltFunction(S905D2_GPIOX(11), S905D2_GPIOX_11_TDMA_SCLK_FN);
  gpio_impl_.SetDriveStrength(S905D2_GPIOX(9), ua, nullptr);
  gpio_impl_.SetDriveStrength(S905D2_GPIOX(10), ua, nullptr);
  gpio_impl_.SetDriveStrength(S905D2_GPIOX(11), ua, nullptr);
#endif

  // PDM pin assignments
  gpio_impl_.SetAltFunction(S905D2_GPIOA(7), S905D2_GPIOA_7_PDM_DCLK_FN);
  gpio_impl_.SetAltFunction(S905D2_GPIOA(8), S905D2_GPIOA_8_PDM_DIN0_FN);

  // Hardware Reset of the codec.
  gpio_impl_.ConfigOut(S905D2_GPIOA(5), 0);
  zx::nanosleep(zx::deadline_after(zx::msec(1)));
  gpio_impl_.ConfigOut(S905D2_GPIOA(5), 1);

  // Output devices.
#ifdef ENABLE_BT
  // Add TDM OUT for BT.
  {
    const std::vector<fpbus::Bti> pcm_out_btis = {
        {{
            .iommu_index = 0,
            .bti_id = BTI_AUDIO_BT_OUT,
        }},
    };
    metadata::AmlConfig metadata = {};
    snprintf(metadata.manufacturer, sizeof(metadata.manufacturer), "Spacely Sprockets");
    snprintf(metadata.product_name, sizeof(metadata.product_name), "astro");
    metadata.is_input = false;
    // Compatible clocks with other TDM drivers.
    metadata.mClockDivFactor = 10;
    metadata.sClockDivFactor = 25;
    metadata.unique_id = AUDIO_STREAM_UNIQUE_ID_BUILTIN_BT;
    metadata.bus = metadata::AmlBus::TDM_A;
    metadata.version = metadata::AmlVersion::kS905D2G;
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

    // Add DAI or controller driver depending on ENABLE_DAI_MODE.
    fpbus::Node tdm_dev;
    tdm_dev.vid() = PDEV_VID_AMLOGIC;
    tdm_dev.pid() = PDEV_PID_AMLOGIC_S905D2;
    tdm_dev.mmio() = audio_mmios;
    tdm_dev.bti() = pcm_out_btis;
    tdm_dev.metadata() = tdm_metadata;
#ifdef ENABLE_DAI_MODE
    tdm_dev.name() = "astro-pcm-dai-out";
    tdm_dev.did() = PDEV_DID_AMLOGIC_DAI_OUT;
    auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, tdm_dev));
    if (!result.ok()) {
      zxlogf(ERROR, "%s: NodeAdd request failed: %s", __func__, result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "%s: NodeAdd failed: %s", __func__,
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
#else
    tdm_dev.name() = "astro-pcm-audio-out";
    tdm_dev.did() = PDEV_DID_AMLOGIC_TDM;
    tdm_dev.instance_id() = tdm_instance_id++;
    tdm_dev.irq() = frddr_a_irqs;

    // TODO(fxb/84194): Migrate to the composite bind rules once dynamic bind rules are
    // available.
    auto result = pbus_.buffer(arena)->AddCompositeImplicitPbusFragment(
        fidl::ToWire(fidl_arena, tdm_dev),
        platform_bus_composite::MakeFidlFragment(fidl_arena, tdm_pcm_fragments,
                                                 std::size(tdm_pcm_fragments)),
        {});
    if (!result.ok()) {
      zxlogf(ERROR, "%s: AddCompositeImplicitPbusFragment Audio(tdm_dev) request failed: %s",
             __func__, result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "%s: AddCompositeImplicitPbusFragment Audio(tdm_dev) failed: %s", __func__,
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
#endif
  }
#endif
  // Add TDM OUT to the codec.
  {
    metadata::ti::TasConfig metadata = {};
#ifdef TAS2770_CONFIG_PATH
    metadata.number_of_writes1 = sizeof(tas2770_init_sequence1) / sizeof(cfg_reg);
    for (size_t i = 0; i < metadata.number_of_writes1; ++i) {
      metadata.init_sequence1[i].address = tas2770_init_sequence1[i].offset;
      metadata.init_sequence1[i].value = tas2770_init_sequence1[i].value;
    }
    metadata.number_of_writes2 = sizeof(tas2770_init_sequence2) / sizeof(cfg_reg);
    for (size_t i = 0; i < metadata.number_of_writes2; ++i) {
      metadata.init_sequence2[i].address = tas2770_init_sequence2[i].offset;
      metadata.init_sequence2[i].value = tas2770_init_sequence2[i].value;
    }
#endif
    const device_metadata_t codec_metadata[] = {
        {
            .type = DEVICE_METADATA_PRIVATE,
            .data = reinterpret_cast<uint8_t*>(&metadata),
            .length = sizeof(metadata),
        },
    };

    status = DdkAddCompositeNodeSpec("audio_codec_tas27xx",
                                     ddk::CompositeNodeSpec(kI2cRules, kI2cProps)
                                         .AddParentSpec(kFaultGpioRules, kFaultGpioProps)
                                         .set_metadata(codec_metadata));
    if (status != ZX_OK) {
      zxlogf(ERROR, "%s DdkAddCompositeNodeSpec failed %d", __FILE__, status);
      return status;
    }
  }
  {
    metadata::AmlConfig metadata = {};
    snprintf(metadata.manufacturer, sizeof(metadata.manufacturer), "Spacely Sprockets");
    snprintf(metadata.product_name, sizeof(metadata.product_name), "astro");
    metadata.is_input = false;
    // Compatible clocks with other TDM drivers.
    metadata.mClockDivFactor = 10;
    metadata.sClockDivFactor = 25;
    metadata.unique_id = AUDIO_STREAM_UNIQUE_ID_BUILTIN_SPEAKERS;
    metadata.bus = metadata::AmlBus::TDM_B;
    metadata.version = metadata::AmlVersion::kS905D2G;
    metadata.dai.type = metadata::DaiType::I2s;
    metadata.dai.bits_per_sample = 16;
    metadata.dai.bits_per_slot = 32;

    // We expose a mono ring buffer to clients. However we still use a 2 channels DAI to the codec
    // so we configure the audio engine to only take the one channel and put it in the right slot
    // going out to the codec via I2S.
    metadata.ring_buffer.number_of_channels = 1;
    metadata.lanes_enable_mask[0] = 1;  // One ring buffer channel goes into the right I2S slot.
    metadata.codecs.number_of_codecs = 1;
    metadata.codecs.channels_to_use_bitmask[0] = 2;  // Codec must use the right I2S slot.

    metadata.codecs.types[0] = metadata::CodecType::Tas27xx;
    // Report our external delay based on the chosen frame rate.  Note that these
    // delays were measured on Astro hardware, and should be pretty good, but they
    // will not be perfect.  One reason for this is that we are not taking any
    // steps to align our start time with start of a TDM frame, which will cause
    // up to 1 frame worth of startup error ever time that the output starts.
    // Also note that this is really nothing to worry about.  Hitting our target
    // to within 20.8uSec (for 48k) is pretty good.
    metadata.codecs.number_of_external_delays = 2;
    metadata.codecs.external_delays[0].frequency = 48'000;
    metadata.codecs.external_delays[0].nsecs = ZX_USEC(125);
    metadata.codecs.external_delays[1].frequency = 96'000;
    metadata.codecs.external_delays[1].nsecs = ZX_NSEC(83333);
    metadata.codecs.ring_buffer_channels_to_use_bitmask[0] = 0x1;  // Single speaker uses index 0.
    metadata.codecs.delta_gains[0] = -1.5f;
    std::vector<fpbus::Metadata> tdm_metadata{
        {{
            .type = DEVICE_METADATA_PRIVATE,
            .data = std::vector<uint8_t>(
                reinterpret_cast<const uint8_t*>(&metadata),
                reinterpret_cast<const uint8_t*>(&metadata) + sizeof(metadata)),
        }},
    };

    fpbus::Node tdm_dev;
    tdm_dev.name() = "astro-i2s-audio-out";
    tdm_dev.vid() = PDEV_VID_AMLOGIC;
    tdm_dev.pid() = PDEV_PID_AMLOGIC_S905D2;
    tdm_dev.did() = PDEV_DID_AMLOGIC_TDM;
    tdm_dev.instance_id() = tdm_instance_id++;
    tdm_dev.mmio() = audio_mmios;
    tdm_dev.bti() = tdm_btis;
    tdm_dev.irq() = frddr_b_irqs;
    tdm_dev.metadata() = tdm_metadata;
    auto tdm_spec = fdf::CompositeNodeSpec{{
        "aml_tdm",
        kTdmI2sSpec,
    }};
    auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(fidl::ToWire(fidl_arena, tdm_dev),
                                                            fidl::ToWire(fidl_arena, tdm_spec));
    if (!result.ok()) {
      zxlogf(ERROR, "AddCompositeNodeSpec request failed: %s", result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "AddCompositeNodeSpec failed: %s", zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  // Input devices.
#ifdef ENABLE_BT
  // Add TDM IN for BT.
  {
    const std::vector<fpbus::Bti> pcm_in_btis{
        {{
            .iommu_index = 0,
            .bti_id = BTI_AUDIO_BT_IN,
        }},
    };
    metadata::AmlConfig metadata = {};
    snprintf(metadata.manufacturer, sizeof(metadata.manufacturer), "Spacely Sprockets");
    snprintf(metadata.product_name, sizeof(metadata.product_name), "astro");
    metadata.is_input = true;
    // Compatible clocks with other TDM drivers.
    metadata.mClockDivFactor = 10;
    metadata.sClockDivFactor = 25;
    metadata.unique_id = AUDIO_STREAM_UNIQUE_ID_BUILTIN_BT;
    metadata.bus = metadata::AmlBus::TDM_A;
    metadata.version = metadata::AmlVersion::kS905D2G;
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
    // Add DAI or controller driver depending on ENABLE_DAI_MODE.
    fpbus::Node tdm_dev;
    tdm_dev.vid() = PDEV_VID_AMLOGIC;
    tdm_dev.pid() = PDEV_PID_AMLOGIC_S905D2;
    tdm_dev.mmio() = audio_mmios;
    tdm_dev.bti() = pcm_in_btis;
    tdm_dev.metadata() = tdm_metadata;
#ifdef ENABLE_DAI_MODE
    tdm_dev.name() = "astro-pcm-dai-in";
    tdm_dev.did() = PDEV_DID_AMLOGIC_DAI_IN;
    auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, tdm_dev));
#else
    tdm_dev.name() = "astro-pcm-audio-in";
    tdm_dev.did() = PDEV_DID_AMLOGIC_TDM;
    tdm_dev.instance_id() = tdm_instance_id++;
    tdm_dev.irq() = toddr_a_irqs;
    auto result = pbus_.buffer(arena)->AddCompositeImplicitPbusFragment(
        fidl::ToWire(fidl_arena, tdm_dev),
        platform_bus_composite::MakeFidlFragment(fidl_arena, tdm_pcm_fragments,
                                                 std::size(tdm_pcm_fragments)),
        nullptr);
#endif
    if (!result.ok()) {
      zxlogf(ERROR, "%s: NodeAdd request failed: %s", __func__, result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "%s: NodeAdd failed: %s", __func__,
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

#endif

  // Input device.
  {
    metadata::AmlPdmConfig metadata = {};
    snprintf(metadata.manufacturer, sizeof(metadata.manufacturer), "Spacely Sprockets");
    snprintf(metadata.product_name, sizeof(metadata.product_name), "astro");
    metadata.number_of_channels = 2;
    metadata.version = metadata::AmlVersion::kS905D2G;
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

    static const std::vector<fpbus::Mmio> pdm_mmios = {
        {{
            .base = S905D2_EE_PDM_BASE,
            .length = S905D2_EE_PDM_LENGTH,
        }},
        {{
            .base = S905D2_EE_AUDIO_BASE,
            .length = S905D2_EE_AUDIO_LENGTH,
        }},
    };

    static const std::vector<fpbus::Bti> pdm_btis{
        {{
            .iommu_index = 0,
            .bti_id = BTI_AUDIO_IN,
        }},
    };

    fpbus::Node dev_in;
    dev_in.name() = "astro-audio-pdm-in";
    dev_in.vid() = PDEV_VID_AMLOGIC;
    dev_in.pid() = PDEV_PID_AMLOGIC_S905D2;
    dev_in.did() = PDEV_DID_AMLOGIC_PDM;
    dev_in.mmio() = pdm_mmios;
    dev_in.bti() = pdm_btis;
    dev_in.irq() = toddr_b_irqs;
    dev_in.metadata() = pdm_metadata;

    auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, dev_in));
    if (!result.ok()) {
      zxlogf(ERROR, "%s: NodeAdd Audio(dev_in) request failed: %s", __func__,
             result.FormatDescription().data());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "%s: NodeAdd Audio(dev_in) failed: %s", __func__,
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }
  return ZX_OK;
}

}  // namespace astro
