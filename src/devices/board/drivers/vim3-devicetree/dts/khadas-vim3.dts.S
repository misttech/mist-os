// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/dts-v1/;

/include/ "amlogic-a311d.dtsi"

#include <lib/ddk/platform-defs.h>

#include "src/devices/lib/amlogic/include/soc/aml-a311d/a311d-gpio.h"

/ {
	compatible = "khadas,vim3", "amlogic,a311d";
	model = "Khadas Vim3";

	dummy_iommu:iommu {
		/* This device has no hardware IOMMU. Use unique BTI id as iommu   */
		/* specifier for each consumer device for bookkeeping system BTIs. */
		#iommu-cells = <0x1>;
	};

	suspend {
		compatible="amlogic,suspend";
	};

	reserved-memory {
		#address-cells = <0x02>;
		#size-cells = <0x02>;
		ranges;

		sysmem: fuchsia,sysmem {
			compatible = "fuchsia,sysmem";
			iommus = <&dummy_iommu 1>;
			pid = <PDEV_PID_AMLOGIC_A311D>;
			vid = <PDEV_VID_AMLOGIC>;
			size = <0x00 0x00>;
			// The AMlogic display engine needs contiguous physical memory for each
			// frame buffer, because it does not have a page table walker. We reserve
			// enough memory to hold 5 framebuffers (2 for virtcon, 3 for scenic) at
			// the maximum supported resolution of 3840 x 2160 with 4 bytes per pixel
			// (for the RGBA/BGRA 8888 pixel format).
			//
			// The maximum supported resolution is documented below.
			// * "A311D Quick Reference Manual" revision 01, pages 2-3
			// * "A311D Datasheet" revision 08, section 2.2 "Features", pages 4-5
			//
			// TODO(https://fxbug.dev/42072489): Reserving this much memory seems wasteful. The
			// quantity below is 10% of RAM on a VIM3 Basic and 5% of RAM on a VIM3 Pro.
			contiguous-size = <0x00 (200*1024*1024)>;
		};
	};

	display_detect: display-detect {
		gpios = <&gpio_expander 0 0>;
		gpio-names = "LCD_RESET";
	};

	backlight {
		gpios = <&gpio_expander 1 0>;
		gpio-names = "LCD_BACKLIGHT_ENABLE";
		pwms = <&pwm 8 0 0>; //PWM_AO_C
		pwm-names = "LCD_BRIGHTNESS";
	};
};

&gpio {
	i2c_AO_pins: i2c-AO-pins {
		pins = <A311D_GPIOAO(2) A311D_GPIOAO(2)>;
		function = <0 A311D_GPIOAO_2_M0_SCL_FN>;
	};

	i2c3_pins: i2c3-pins {
		pins = <A311D_GPIOA(15) A311D_GPIOA(14)>;
		function = <0 A311D_GPIOA_15_I2C_EE_M3_SCL_FN>;
	};
};

&i2c_AO {
	pinctrl-0 = <&i2c_AO_pins>;

	gpio_expander: gpio-controller@20 {
		compatible = "ti,tca6408a";
		reg = <0x20>;
		gpio-controller;
		#gpio-cells = <2>;
	};

	khadas-mcu@18 {
		compatible = "khadas,vim3-mcu";
		reg = <0x18>;
	};

	fusb302@22 {
		reg = <0x22>;
		gpios = <&gpio A311D_GPIOAO(8) 0>;
		gpio-names = "USB_POWER_DELIVERY";
	};

	rtc@51 {
		compatible = "nxp,pcf8563";
		reg = <0x51>;
	};
};

&i2c3 {
	pinctrl-0 = <&i2c3_pins>;
};
