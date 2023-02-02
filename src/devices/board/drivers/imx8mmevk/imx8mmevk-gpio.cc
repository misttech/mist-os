// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "imx8mmevk-gpio.h"

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <ddk/metadata/gpio.h>

#include "imx8mmevk.h"
#include "src/devices/lib/nxp/include/soc/imx8m/gpio.h"
#include "src/devices/lib/nxp/include/soc/imx8mm/gpio.h"

namespace imx8mm_evk {
namespace fpbus = fuchsia_hardware_platform_bus;

zx_status_t Imx8mmEvk::GpioInit() {
  imx8m::PinConfigMetadata pinconfig_metadata = {};

  // USDHC3 - eMMC pin configuration
  pinconfig_metadata.pin_config_entry[0] = {
      IOMUXC_NAND_WE_B_USDHC3_CLK,
      (PE(0b1) | HYS(0b1) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[1] = {
      IOMUXC_NAND_WP_B_USDHC3_CMD,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[2] = {
      IOMUXC_NAND_DATA04_USDHC3_DATA0,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[3] = {
      IOMUXC_NAND_DATA05_USDHC3_DATA1,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[4] = {
      IOMUXC_NAND_DATA06_USDHC3_DATA2,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[5] = {
      IOMUXC_NAND_DATA07_USDHC3_DATA3,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[6] = {
      IOMUXC_NAND_RE_B_USDHC3_DATA4,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[7] = {
      IOMUXC_NAND_CE2_B_USDHC3_DATA5,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[8] = {
      IOMUXC_NAND_CE3_B_USDHC3_DATA6,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[9] = {
      IOMUXC_NAND_CLE_USDHC3_DATA7,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[10] = {
      IOMUXC_NAND_CE1_B_USDHC3_STROBE,
      (PE(0b1) | HYS(0b1) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  // I2C1
  pinconfig_metadata.pin_config_entry[11] = {
      IOMUXC_I2C1_SCL_I2C1_SCL,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b00) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[12] = {
      IOMUXC_I2C1_SDA_I2C1_SDA,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b00) | DSE(0b110)),
  };

  // I2C2
  pinconfig_metadata.pin_config_entry[13] = {
      IOMUXC_I2C2_SCL_I2C2_SCL,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b00) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[14] = {
      IOMUXC_I2C2_SDA_I2C2_SDA,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b00) | DSE(0b110)),
  };

  // I2C3
  pinconfig_metadata.pin_config_entry[15] = {
      IOMUXC_I2C3_SCL_I2C3_SCL,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b00) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[16] = {
      IOMUXC_I2C3_SDA_I2C3_SDA,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b00) | DSE(0b110)),
  };

  // UART1
  pinconfig_metadata.pin_config_entry[17] = {
      IOMUXC_UART1_RXD_UART1_RX,
      (PE(0b1) | HYS(0b0) | PUE(0b1) | ODE(0b0) | FSEL(0b00) | DSE(0b000)),
  };

  pinconfig_metadata.pin_config_entry[18] = {
      IOMUXC_UART1_TXD_UART1_TX,
      (PE(0b1) | HYS(0b0) | PUE(0b1) | ODE(0b0) | FSEL(0b00) | DSE(0b000)),
  };

  pinconfig_metadata.pin_config_entry[19] = {
      IOMUXC_UART3_RXD_UART1_CTS_B,
      (PE(0b1) | HYS(0b0) | PUE(0b1) | ODE(0b0) | FSEL(0b00) | DSE(0b000)),
  };

  pinconfig_metadata.pin_config_entry[20] = {
      IOMUXC_UART3_TXD_UART1_RTS_B,
      (PE(0b1) | HYS(0b0) | PUE(0b1) | ODE(0b0) | FSEL(0b00) | DSE(0b000)),
  };

  // UART2
  pinconfig_metadata.pin_config_entry[21] = {
      IOMUXC_UART2_RXD_UART2_RX,
      (PE(0b1) | HYS(0b0) | PUE(0b1) | ODE(0b0) | FSEL(0b00) | DSE(0b000)),
  };

  pinconfig_metadata.pin_config_entry[22] = {
      IOMUXC_UART2_TXD_UART2_TX,
      (PE(0b1) | HYS(0b0) | PUE(0b1) | ODE(0b0) | FSEL(0b00) | DSE(0b000)),
  };

  // UART3
  pinconfig_metadata.pin_config_entry[23] = {
      IOMUXC_ECSPI1_SCLK_UART3_RX,
      (PE(0b1) | HYS(0b0) | PUE(0b1) | ODE(0b0) | FSEL(0b00) | DSE(0b000)),
  };

  pinconfig_metadata.pin_config_entry[24] = {
      IOMUXC_ECSPI1_MOSI_UART3_TX,
      (PE(0b1) | HYS(0b0) | PUE(0b1) | ODE(0b0) | FSEL(0b00) | DSE(0b000)),
  };

  pinconfig_metadata.pin_config_entry[25] = {
      IOMUXC_ECSPI1_SS0_UART3_RTS_B,
      (PE(0b1) | HYS(0b0) | PUE(0b1) | ODE(0b0) | FSEL(0b00) | DSE(0b000)),
  };

  pinconfig_metadata.pin_config_entry[26] = {
      IOMUXC_ECSPI1_MISO_UART3_CTS_B,
      (PE(0b1) | HYS(0b0) | PUE(0b1) | ODE(0b0) | FSEL(0b00) | DSE(0b000)),
  };

  // SAI3
  pinconfig_metadata.pin_config_entry[27] = {
      IOMUXC_SAI3_TXFS_SAI3_TX_SYNC,
      (PE(0b0) | HYS(0b1) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[28] = {
      IOMUXC_SAI3_TXC_SAI3_TX_BCLK,
      (PE(0b0) | HYS(0b1) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[29] = {
      IOMUXC_SAI3_MCLK_SAI3_MCLK,
      (PE(0b0) | HYS(0b1) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[30] = {
      IOMUXC_SAI3_TXD_SAI3_TX_DATA0,
      (PE(0b0) | HYS(0b1) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  // ENET
  pinconfig_metadata.pin_config_entry[31] = {
      IOMUXC_ENET_MDC_ENET1_MDC,
      (PE(0b0) | HYS(0b0) | PUE(0b0) | ODE(0b0) | FSEL(0b00) | DSE(0b010)),
  };

  pinconfig_metadata.pin_config_entry[32] = {
      IOMUXC_ENET_MDIO_ENET1_MDIO,
      (PE(0b0) | HYS(0b0) | PUE(0b0) | ODE(0b0) | FSEL(0b00) | DSE(0b010)),
  };

  pinconfig_metadata.pin_config_entry[33] = {
      IOMUXC_ENET_TD3_ENET1_RGMII_TD3,
      (PE(0b0) | HYS(0b0) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[34] = {
      IOMUXC_ENET_TD2_ENET1_RGMII_TD2,
      (PE(0b0) | HYS(0b0) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[35] = {
      IOMUXC_ENET_TD1_ENET1_RGMII_TD1,
      (PE(0b0) | HYS(0b0) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[36] = {
      IOMUXC_ENET_TD0_ENET1_RGMII_TD0,
      (PE(0b0) | HYS(0b0) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[37] = {
      IOMUXC_ENET_RD3_ENET1_RGMII_RD3,
      (PE(0b0) | HYS(0b1) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b000)),
  };

  pinconfig_metadata.pin_config_entry[38] = {
      IOMUXC_ENET_RD2_ENET1_RGMII_RD2,
      (PE(0b0) | HYS(0b1) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b000)),
  };

  pinconfig_metadata.pin_config_entry[39] = {
      IOMUXC_ENET_RD1_ENET1_RGMII_RD1,
      (PE(0b0) | HYS(0b1) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b000)),
  };

  pinconfig_metadata.pin_config_entry[40] = {
      IOMUXC_ENET_RD0_ENET1_RGMII_RD0,
      (PE(0b0) | HYS(0b1) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b000)),
  };

  pinconfig_metadata.pin_config_entry[41] = {
      IOMUXC_ENET_TXC_ENET1_RGMII_TXC,
      (PE(0b0) | HYS(0b0) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[42] = {
      IOMUXC_ENET_RXC_ENET1_RGMII_RXC,
      (PE(0b0) | HYS(0b1) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b000)),
  };

  pinconfig_metadata.pin_config_entry[43] = {
      IOMUXC_ENET_RX_CTL_ENET1_RGMII_RX_CTL,
      (PE(0b0) | HYS(0b1) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b000)),
  };

  pinconfig_metadata.pin_config_entry[44] = {
      IOMUXC_ENET_TX_CTL_ENET1_RGMII_TX_CTL,
      (PE(0b0) | HYS(0b0) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[45] = {
      IOMUXC_SAI2_RXC_GPIO4_IO22,
      (PE(0b0) | HYS(0b0) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b000)),
  };

  // LED
  pinconfig_metadata.pin_config_entry[46] = {
      IOMUXC_NAND_READY_B_GPIO3_IO16,
      (PE(0b0) | HYS(0b0) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b000)),
  };

  // PMIC
  pinconfig_metadata.pin_config_entry[47] = {
      IOMUXC_GPIO1_IO03_GPIO1_IO03,
      (PE(0b1) | HYS(0b0) | PUE(0b1) | ODE(0b0) | FSEL(0b00) | DSE(0b000)),
  };

  // MIPI-DSI-EN
  pinconfig_metadata.pin_config_entry[48] = {
      IOMUXC_GPIO1_IO08_GPIO1_IO08,
      (PE(0b0) | HYS(0b0) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  // USB type-C
  pinconfig_metadata.pin_config_entry[49] = {
      IOMUXC_SD1_STROBE_GPIO2_IO11,
      (PE(0b0) | HYS(0b0) | PUE(0b0) | ODE(0b1) | FSEL(0b00) | DSE(0b000)),
  };

  // USDHC2 - SD card pin configuration
  pinconfig_metadata.pin_config_entry[50] = {
      IOMUXC_SD2_CLK_USDHC2_CLK,
      (PE(0b1) | HYS(0b1) | PUE(0b0) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[51] = {
      IOMUXC_SD2_CMD_USDHC2_CMD,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[52] = {
      IOMUXC_SD2_DATA0_USDHC2_DATA0,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[53] = {
      IOMUXC_SD2_DATA1_USDHC2_DATA1,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[54] = {
      IOMUXC_SD2_DATA2_USDHC2_DATA2,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[55] = {
      IOMUXC_SD2_DATA3_USDHC2_DATA3,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b10) | DSE(0b110)),
  };

  pinconfig_metadata.pin_config_entry[56] = {
      IOMUXC_GPIO1_IO04_USDHC2_VSELECT,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b10) | DSE(0b000)),
  };

  pinconfig_metadata.pin_config_entry[57] = {
      IOMUXC_GPIO1_IO15_GPIO1_IO15,
      (PE(0b1) | HYS(0b1) | PUE(0b1) | ODE(0b0) | FSEL(0b00) | DSE(0b100)),
  };

  pinconfig_metadata.pin_config_entry_count = 58;

  // gpio port doesn't necessarily have 32 pins, so maintain pin count
  // per port so that GPIO APIs can validate input pin number.
  pinconfig_metadata.port_info[0].pin_count = imx8mm::kGpio1PinCount;
  pinconfig_metadata.port_info[1].pin_count = imx8mm::kGpio2PinCount;
  pinconfig_metadata.port_info[2].pin_count = imx8mm::kGpio3PinCount;
  pinconfig_metadata.port_info[3].pin_count = imx8mm::kGpio4PinCount;
  pinconfig_metadata.port_info[4].pin_count = imx8mm::kGpio5PinCount;

  static const std::vector<fpbus::Mmio> gpio_mmios{
      {{
          .base = imx8mm::kIOMUXCBase,
          .length = imx8mm::kIOMUXCSize,
      }},
      {{
          .base = imx8mm::kGpio1Base,
          .length = imx8mm::kGpioSize,
      }},
      {{
          .base = imx8mm::kGpio2Base,
          .length = imx8mm::kGpioSize,
      }},
      {{
          .base = imx8mm::kGpio3Base,
          .length = imx8mm::kGpioSize,
      }},
      {{
          .base = imx8mm::kGpio4Base,
          .length = imx8mm::kGpioSize,
      }},
      {{
          .base = imx8mm::kGpio5Base,
          .length = imx8mm::kGpioSize,
      }},
  };

  static const std::vector<fpbus::Irq> gpio_irqs{
      {{
          .irq = imx8mm::kGpio1Irq0to15,
          .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
      }},
      {{
          .irq = imx8mm::kGpio1Irq16to31,
          .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
      }},
      {{
          .irq = imx8mm::kGpio2Irq0to15,
          .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
      }},
      {{
          .irq = imx8mm::kGpio2Irq16to31,
          .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
      }},
      {{
          .irq = imx8mm::kGpio3Irq0to15,
          .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
      }},
      {{
          .irq = imx8mm::kGpio3Irq16to31,
          .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
      }},
      {{
          .irq = imx8mm::kGpio4Irq0to15,
          .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
      }},
      {{
          .irq = imx8mm::kGpio4Irq16to31,
          .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
      }},
      {{
          .irq = imx8mm::kGpio5Irq0to15,
          .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
      }},
      {{
          .irq = imx8mm::kGpio5Irq16to31,
          .mode = ZX_INTERRUPT_MODE_LEVEL_HIGH,
      }},
  };

  const gpio_pin_t gpio_pins[] = {
      DECL_GPIO_PIN(IMX8MMEVK_LED),
  };

  std::vector<fpbus::Metadata> gpio_metadata{
      {{
          .type = DEVICE_METADATA_GPIO_PINS,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&gpio_pins),
              reinterpret_cast<const uint8_t*>(&gpio_pins) + sizeof(gpio_pins)),
      }},
      {{
          .type = DEVICE_METADATA_PRIVATE,
          .data = std::vector<uint8_t>(
              reinterpret_cast<const uint8_t*>(&pinconfig_metadata),
              reinterpret_cast<const uint8_t*>(&pinconfig_metadata) + sizeof(pinconfig_metadata)),
      }},
  };

  fpbus::Node gpio_dev;
  gpio_dev.name() = "gpio";
  gpio_dev.vid() = PDEV_VID_NXP;
  gpio_dev.pid() = PDEV_PID_IMX8MMEVK;
  gpio_dev.did() = PDEV_DID_IMX_GPIO;
  gpio_dev.mmio() = gpio_mmios;
  gpio_dev.irq() = gpio_irqs;
  gpio_dev.metadata() = gpio_metadata;

  fidl::Arena<> fidl_arena;
  fdf::Arena arena('GPIO');
  auto result = pbus_.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, gpio_dev));
  if (!result.ok()) {
    zxlogf(ERROR, "NodeAdd Gpio(gpio_dev) request failed: %s", result.FormatDescription().data());
    return result.status();
  }
  if (result->is_error()) {
    zxlogf(ERROR, "NodeAdd Gpio(gpio_dev) failed: %s", zx_status_get_string(result->error_value()));
    return result->error_value();
  }
  return ZX_OK;
}

}  // namespace imx8mm_evk
