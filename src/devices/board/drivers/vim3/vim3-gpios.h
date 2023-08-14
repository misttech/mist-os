// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BOARD_DRIVERS_VIM3_VIM3_GPIOS_H_
#define SRC_DEVICES_BOARD_DRIVERS_VIM3_VIM3_GPIOS_H_

#include <soc/aml-a311d/a311d-gpio.h>

// VIM3 specific assignments should be placed in this file.
// SoC specific definitions should be placed in soc/aml-a311d-gpio.h

// Ethernet
#define VIM3_ETH_MAC_INTR A311D_GPIOZ(14)

// 40-pin J4 header
#define VIM3_J4_PIN_39 A311D_GPIOZ(15)

// SDIO
#define A311D_SDIO_D0 A311D_GPIOX(0)
#define A311D_SDIO_D1 A311D_GPIOX(1)
#define A311D_SDIO_D2 A311D_GPIOX(2)
#define A311D_SDIO_D3 A311D_GPIOX(3)
#define A311D_SDIO_CLK A311D_GPIOX(4)
#define A311D_SDIO_CMD A311D_GPIOX(5)

// Audio
#define VIM3_I2SB_SCLK A311D_GPIOA(1)
#define VIM3_I2SB_LRCLK A311D_GPIOA(2)
#define VIM3_I2SB_SDO A311D_GPIOA(3)
#define VIM3_I2SB_SDI A311D_GPIOA(4)

#define VIM3_BTPCM_CLK A311D_GPIOX(11)
#define VIM3_BTPCM_SYNC A311D_GPIOX(10)
#define VIM3_BTPCM_OUT A311D_GPIOX(9)
#define VIM3_BTPCM_IN A311D_GPIOX(8)

// Display
#define VIM3_HPD_IN A311D_GPIOH(2)

// USB Power Delivery (Fusb302)
#define VIM3_FUSB302_INT A311D_GPIOAO(8)

#define VIM3_TOUCH_PANEL_INTERRUPT A311D_GPIOA(5)

// USB Power Enable for USB 2.0 and USB 3.0 ports
#define VIM3_USB_PWR A311D_GPIOA(6)

// WIFI
#define VIM3_WIFI_WAKE_HOST A311D_GPIOX(7)
#define VIM3_WIFI_32K A311D_GPIOX(16)

// BT
#define VIM3_BT_EN A311D_GPIOX(17)

// Pin alternate functions
#define A311D_GPIOA_1_TDMB_SCLK_FN 1
#define A311D_GPIOA_1_TDMB_SLV_SCLK_FN 2
#define A311D_GPIOA_2_TDMB_FS_FN 1
#define A311D_GPIOA_2_TDMB_SLV_FS_FN 2
#define A311D_GPIOA_3_TDMB_D0_FN 1
#define A311D_GPIOA_3_TDMB_DIN0_FN 2
#define A311D_GPIOA_4_TDMB_D1_FN 1
#define A311D_GPIOA_4_TDMB_DIN1_FN 2
#define A311D_GPIOX_8_TDMA_D1_FN 1
#define A311D_GPIOX_8_TDMA_DIN1_FN 2
#define A311D_GPIOX_9_TDMA_D0_FN 1
#define A311D_GPIOX_9_TDMA_DIN0_FN 2
#define A311D_GPIOX_10_TDMA_FS_FN 1
#define A311D_GPIOX_11_TDMA_SCLK_FN 1

// Make the GPIO expander indices start after the SoC GPIOs to avoid any overlap.
#define VIM3_EXPANDER_GPIO_START (A311D_GPIOE_START + A311D_GPIOE_COUNT)
#define VIM3_EXPANDER_GPIO(n) (VIM3_EXPANDER_GPIO_START + (n))

#define VIM3_LCD_RESET VIM3_EXPANDER_GPIO(0)

// This pin is named LCD_EN on VIM3 V14 Schematics [1] (Page 6). Though per
// the TS050 touchscreen V13 schematics [2], the pin actually only controls the
// power of the backlight module; the LCD panel is always powered. Thus, we
// rename the pin based on its actual behavior.
//
// [1] https://dl.khadas.com/products/vim3/schematic/vim3_sch_v14.pdf
// [1] https://dl.khadas.com/hardware/Accessories/TS050/TS050_V13_Sch.pdf
#define VIM3_LCD_BACKLIGHT_ENABLE VIM3_EXPANDER_GPIO(1)
#define VIM3_TOUCH_PANEL_RESET VIM3_EXPANDER_GPIO(6)
#define VIM3_SD_MODE VIM3_EXPANDER_GPIO(7)

#endif  // SRC_DEVICES_BOARD_DRIVERS_VIM3_VIM3_GPIOS_H_
