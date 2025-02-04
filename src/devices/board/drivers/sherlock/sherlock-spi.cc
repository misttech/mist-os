// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fidl/fuchsia.hardware.spi.businfo/cpp/fidl.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/mmio/mmio.h>

#include <bind/fuchsia/amlogic/platform/cpp/bind.h>
#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/gpio/cpp/bind.h>
#include <bind/fuchsia/hardware/registers/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <bind/fuchsia/register/cpp/bind.h>
#include <fbl/algorithm.h>
#include <soc/aml-common/aml-registers.h>
#include <soc/aml-common/aml-spi.h>
#include <soc/aml-t931/t931-gpio.h>

#include "sherlock-gpios.h"
#include "sherlock.h"
#include "src/devices/lib/fidl-metadata/spi.h"

#define HHI_SPICC_CLK_CNTL (0xf7 * 4)
#define spicc_0_clk_sel_fclk_div3 (3 << 7)
#define spicc_0_clk_en (1 << 6)
#define spicc_0_clk_div(x) ((x) - 1)

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace sherlock {
namespace fpbus = fuchsia_hardware_platform_bus;
using spi_channel_t = fidl_metadata::spi::Channel;

static const std::vector<fpbus::Mmio> spi_mmios{
    {{
        .base = T931_SPICC0_BASE,
        .length = 0x44,
    }},
};

static const std::vector<fpbus::Irq> spi_irqs{
    {{
        .irq = T931_SPICC0_IRQ,
        .mode = fpbus::ZirconInterruptMode::kEdgeHigh,
    }},
};

static const spi_channel_t spi_channels[] = {
    // Thread SPI
    {
        .cs = 0,  // index into matching chip-select map
        .vid = PDEV_VID_NORDIC,
        .pid = PDEV_PID_NORDIC_NRF52840,
        .did = PDEV_DID_NORDIC_THREAD,
    },
};

static const amlogic_spi::amlspi_config_t spi_config = {
    .bus_id = SHERLOCK_SPICC0,
    .cs_count = 1,
    .cs = {0},                                       // index into fragments list
    .clock_divider_register_value = (512 >> 1) - 1,  // SCLK = core clock / 512 = ~1.3 MHz
    .use_enhanced_clock_mode = true,
};

const std::vector kGpioSpiRules = {
    fdf::MakeAcceptBindRule(bind_fuchsia_hardware_gpio::SERVICE,
                            bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule(bind_fuchsia::GPIO_PIN, static_cast<uint32_t>(GPIO_SPICC0_SS0)),
};

const std::vector kGpioSpiProperties = {
    fdf::MakeProperty(bind_fuchsia_hardware_gpio::SERVICE,
                      bind_fuchsia_hardware_gpio::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty(bind_fuchsia_gpio::FUNCTION, bind_fuchsia_gpio::FUNCTION_SPICC0_SS0),
};

const std::vector kResetRegisterRules = {
    fdf::MakeAcceptBindRule(bind_fuchsia_hardware_registers::SERVICE,
                            bind_fuchsia_hardware_registers::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeAcceptBindRule(bind_fuchsia_register::NAME,
                            bind_fuchsia_amlogic_platform::NAME_REGISTER_SPICC0_RESET),
};

const std::vector kResetRegisterProperties = {
    fdf::MakeProperty(bind_fuchsia_hardware_registers::SERVICE,
                      bind_fuchsia_hardware_registers::SERVICE_ZIRCONTRANSPORT),
    fdf::MakeProperty(bind_fuchsia_register::NAME,
                      bind_fuchsia_amlogic_platform::NAME_REGISTER_SPICC0_RESET),
};

const std::vector<fdf::BindRule> kGpioInitRules = std::vector{
    fdf::MakeAcceptBindRule(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};

const std::vector<fdf::NodeProperty> kGpioInitProperties = std::vector{
    fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
};

zx_status_t Sherlock::SpiInit() {
  // setup pinmux for the SPI bus
  // SPI_A
  gpio_init_steps_.push_back(GpioFunction(T931_GPIOC(0), 5));     // MOSI
  gpio_init_steps_.push_back(GpioFunction(T931_GPIOC(1), 5));     // MISO
  gpio_init_steps_.push_back(GpioOutput(GPIO_SPICC0_SS0, true));  // SS0
  gpio_init_steps_.push_back(fuchsia_hardware_pinimpl::InitStep::WithCall({{
      // SCLK
      .pin = T931_GPIOC(3),
      .call = fuchsia_hardware_pinimpl::InitCall::WithPinConfig({{
          .pull = fuchsia_hardware_pin::Pull::kDown,
          .function = 5,
      }}),
  }}));

  std::vector<uint8_t> encoded_spi_bus_metadata;
  {
    zx::result result = fidl_metadata::spi::SpiChannelsToFidl(SHERLOCK_SPICC0, spi_channels);
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to encode spi channels to fidl: %s", result.status_string());
      return result.error_value();
    }
    encoded_spi_bus_metadata = std::move(result.value());
  }

  std::vector<fpbus::Metadata> spi_metadata{
      {{.id = std::to_string(DEVICE_METADATA_AMLSPI_CONFIG),
        .data = std::vector<uint8_t>(
            reinterpret_cast<const uint8_t*>(&spi_config),
            reinterpret_cast<const uint8_t*>(&spi_config) + sizeof(spi_config))}},
      // TODO(b/392676138): Remove once no longer referenced.
      {{.id = std::to_string(DEVICE_METADATA_SPI_CHANNELS), .data = encoded_spi_bus_metadata}},
      {{.id = fuchsia_hardware_spi_businfo::SpiBusMetadata::kSerializableName,
        .data = std::move(encoded_spi_bus_metadata)}},
  };

  fpbus::Node spi_dev;
  spi_dev.name() = "spi-0";
  spi_dev.vid() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_VID_AMLOGIC;
  spi_dev.pid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC;
  spi_dev.did() = bind_fuchsia_amlogic_platform::BIND_PLATFORM_DEV_DID_SPI;
  spi_dev.mmio() = spi_mmios;
  spi_dev.irq() = spi_irqs;
  spi_dev.metadata() = std::move(spi_metadata);

  // TODO(https://fxbug.dev/42109271): fix this clock enable block when the clock driver can handle
  // the dividers
  {
    zx::unowned_resource resource(get_mmio_resource(parent()));
    zx::vmo vmo;
    zx_status_t status = zx::vmo::create_physical(*resource, T931_HIU_BASE, T931_HIU_LENGTH, &vmo);
    if (status != ZX_OK) {
      zxlogf(ERROR, "failed to create VMO: %s", zx_status_get_string(status));
      return status;
    }
    zx::result<fdf::MmioBuffer> buf = fdf::MmioBuffer::Create(0, T931_HIU_LENGTH, std::move(vmo),
                                                              ZX_CACHE_POLICY_UNCACHED_DEVICE);
    if (buf.is_error()) {
      zxlogf(ERROR, "fdf::MmioBuffer::Create() error: %s", buf.status_string());
      return buf.status_value();
    }

    // SPICC0 clock enable (666 MHz)
    buf->Write32(spicc_0_clk_sel_fclk_div3 | spicc_0_clk_en | spicc_0_clk_div(1),
                 HHI_SPICC_CLK_CNTL);
  }

  auto parents = std::vector<fdf::ParentSpec>{
      {kGpioSpiRules, kGpioSpiProperties},
      {kResetRegisterRules, kResetRegisterProperties},
      {kGpioInitRules, kGpioInitProperties},
  };

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('SPI_');
  auto result = pbus_.buffer(arena)->AddCompositeNodeSpec(
      fidl::ToWire(fidl_arena, spi_dev),
      fidl::ToWire(fidl_arena, fdf::CompositeNodeSpec{{.name = "spi_0", .parents = parents}}));
  if (!result.ok()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Spi(spi_dev) request failed: %s",
           result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "AddCompositeNodeSpec Spi(spi_dev) failed: %s",
           zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  return ZX_OK;
}

}  // namespace sherlock
