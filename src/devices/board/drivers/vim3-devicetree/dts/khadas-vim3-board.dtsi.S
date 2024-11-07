// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/dts-v1/;

/include/ "amlogic-a311d.dtsi"

#include <lib/ddk/platform-defs.h>
#include <soc/aml-a311d/a311d-hw.h>
#include <soc/aml-a311d/a311d-gpio.h>

#include "sdk/lib/driver/devicetree/visitors/drivers/interrupt-controllers/arm-gic/arm-gicv2.h"

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
			iommus = <&dummy_iommu 14>;
			pid = <PDEV_PID_AMLOGIC_A311D>;
			vid = <PDEV_VID_AMLOGIC>;
			size = <0x00 0x00>;
		};
	};

	cpu_opp_table0: opp-table-0 {
		opp-1000000000 {
			opp-hz = <0 1000000000>;
			opp-microvolt = <761000>;
		};
		opp-1200000000 {
			opp-hz = <0 1200000000>;
			opp-microvolt = <781000>;
		};
		opp-1398000000 {
			opp-hz = <0 1398000000>;
			opp-microvolt = <811000>;
		};
		opp-1512000000 {
			opp-hz = <0 1512000000>;
			opp-microvolt = <861000>;
		};
		opp-1608000000 {
			opp-hz = <0 1608000000>;
			opp-microvolt = <901000>;
		};
		opp-1704000000 {
			opp-hz = <0 1704000000>;
			opp-microvolt = <951000>;
		};
		opp-1800000000 {
			opp-hz = <0 1800000000>;
			opp-microvolt = <1001000>;
		};
	};

	cpu_opp_table1: opp-table-1 {
		opp-1000000000 {
			opp-hz = <0 1000000000>;
			opp-microvolt = <731000>;
		};
		opp-1200000000 {
			opp-hz = <0 1200000000>;
			opp-microvolt = <751000>;
		};
		opp-1398000000 {
			opp-hz = <0 1398000000>;
			opp-microvolt = <771000>;
		};
		opp-1512000000 {
			opp-hz = <0 1512000000>;
			opp-microvolt = <771000>;
		};
		opp-1608000000 {
			opp-hz = <0 1608000000>;
			opp-microvolt = <781000>;
		};
		opp-1704000000 {
			opp-hz = <0 1704000000>;
			opp-microvolt = <791000>;
		};
		opp-1800000000 {
			opp-hz = <0 1800000000>;
			opp-microvolt = <831000>;
		};
		opp-1908000000 {
			opp-hz = <0 1908000000>;
			opp-microvolt = <861000>;
		};
		opp-2016000000 {
			opp-hz = <0 2016000000>;
			opp-microvolt = <911000>;
		};
		opp-2108000000 {
			opp-hz = <0 2108000000>;
			opp-microvolt = <951000>;
		};
		opp-2208000000 {
			opp-hz = <0 2208000000>;
			opp-microvolt = <1011000>;
		};
	};

	display_detect: display-detect {
		gpios = <&gpio_expander 0 0>;
		gpio-names = "LCD_RESET";
		#display-detect-cells = <0>;
	};

	backlight {
		gpios = <&gpio_expander 1 0>;
		// This gpio pin is named LCD_EN on VIM3 V14 Schematics [1] (Page 6). Though per
		// the TS050 touchscreen V13 schematics [2], the pin actually only controls the
		// power of the backlight module; the LCD panel is always powered. Thus, we
		// rename the pin based on its actual behavior.
		//
		// [1] https://dl.khadas.com/products/vim3/schematic/vim3_sch_v14.pdf
		// [1] https://dl.khadas.com/hardware/Accessories/TS050/TS050_V13_Sch.pdf
		gpio-names = "LCD_BACKLIGHT_ENABLE";
		pwms = <&pwm 8 0 0>; //PWM_AO_C
		pwm-names = "LCD_BRIGHTNESS";
		display-detect = <&display_detect>;
		display-detect-names = "MIPI_DSI";
	};

	dsi-display@ff900000 {
		compatible = "amlogic,display";
		reg = <0x0 A311D_VPU_BASE 0x0 A311D_VPU_LENGTH>,
			<0x0 A311D_TOP_MIPI_DSI_BASE 0x0 A311D_TOP_MIPI_DSI_LENGTH>,
			<0x0 A311D_DSI_PHY_BASE 0x0 A311D_DSI_PHY_LENGTH>,
			<0x0 A311D_MIPI_DSI_BASE 0x0 A311D_MIPI_DSI_LENGTH>,
			<0x0 A311D_HIU_BASE 0x0 A311D_HIU_LENGTH>,
			<0x0 A311D_AOBUS_BASE 0x0 A311D_AOBUS_LENGTH>,
			<0x0 A311D_RESET_BASE 0x0 A311D_RESET_LENGTH>,
			<0x0 A311D_GPIO_BASE 0x0 A311D_GPIO_LENGTH>,
			<0x0 A311D_HDMITX_CONTROLLER_IP_BASE 0x0 A311D_HDMITX_CONTROLLER_IP_LENGTH>,
			<0x0 A311D_HDMITX_TOP_LEVEL_BASE 0x0 A311D_HDMITX_TOP_LEVEL_LENGTH>;
		interrupts = <GIC_SPI 3 GIC_IRQ_MODE_EDGE_RISING>,
			<GIC_SPI 89 GIC_IRQ_MODE_EDGE_RISING>,
			<GIC_SPI 86 GIC_IRQ_MODE_EDGE_RISING>;
		iommus = <&dummy_iommu 13>;
		smcs = <2 1 0>;
		gpios = <&gpio_expander 0 0>,
			<&gpio A311D_GPIOH(2) 0>;
		gpio-names = "LCD_RESET", "HDMI_HOTPLUG_DETECT";
		pinctrl-0 = <&hdmi_hotplug_detect_pins>;
		amlogic,canvas = <&canvas>;
		display-detect = <&display_detect>;
		display-detect-names = "MIPI_DSI";
		panel-type = <0xc>;
		display-width = <1080>;
		display-height = <1920>;
	};

	function_button: adc-buttons {
		compatible = "fuchsia,adc-buttons";
		io-channels = <&saradc 2>;
		io-channel-names = "VIM3_ADC_BUTTON";
	};

	bt_uart: bt-uart@ffd24000 {
		compatible = "amlogic,bt-uart";
		reg = <0x0 A311D_UART_EE_A_BASE 0x0 A311D_UART_EE_A_LENGTH>;
		interrupts = <GIC_SPI 26 GIC_IRQ_MODE_EDGE_RISING>;
		serial-port = <2 6 3>;
		boot-metadata = <0x43414D6D 0>;
		pinctrl-0 = <&bt_uart_pins>;
		pwm-init = <&pwm_init>;
	};

	power_button: gpio-buttons {
		compatible = "fuchsia,gpio-buttons";
		gpios = <&gpio A311D_GPIOAO(7) 0>;
		gpio-names = "POWER";
		pinctrl-0 = <&gpio_buttons_pins>;
	};

	wifi {
		compatible = "broadcom,bcm4359";
		gpios = <&gpio A311D_GPIOX(7) 0>;
		gpio-names = "WIFI_WAKE_HOST";
		pinctrl-0 = <&wifi_pins>;
	};

	gpio_header {
		gpios = <&gpio A311D_GPIOH(4) 0>;
		gpio-names = "GPIO_HEADER_PIN_37";
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

	bt_uart_pins: bt-uart-pins {
		pins = <A311D_GPIOX(12) A311D_GPIOX(13) A311D_GPIOX(14) A311D_GPIOX(15)>;
		function = <0 1>;
	};

	gpio_buttons_pins: gpio-buttons-pins {
		pins = <A311D_GPIOAO(7)>;
		bias-disable;
		function = <0 0>;
	};

	fusb302_pins: fusb302-pins {
		pins = <A311D_GPIOAO(8)>;
		bias-pull-up;
		output-disable;
	};

	wifi_pins: wifi-pins {
		pins = <A311D_GPIOX(7)>;
		bias-disable;
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
		pinctrl-0 = <&fusb302_pins>;
	};

	rtc@51 {
		compatible = "nxp,pcf8563";
		reg = <0x51>;
	};
};

&i2c3 {
	pinctrl-0 = <&i2c3_pins>;
};
