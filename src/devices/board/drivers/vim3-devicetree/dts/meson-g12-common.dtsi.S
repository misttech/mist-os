// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/dts-v1/;

#include "src/devices/lib/amlogic/include/soc/aml-a311d/a311d-hw.h"
#include "src/devices/lib/amlogic/include/soc/aml-a311d/a311d-gpio.h"
#include "sdk/lib/driver/devicetree/visitors/drivers/interrupt-controllers/arm-gicv2/arm-gicv2.h"

/ {
	interrupt-parent = <&gic>;

	soc {
		#address-cells = <2>;
		#size-cells = <2>;
		ranges;

		gpio: gpio-controller@ff634400 {
			compatible = "amlogic,meson-g12a-gpio";
			reg = <0x0 A311D_GPIO_BASE 0x0 A311D_GPIO_LENGTH>,
				<0x0 A311D_GPIO_AO_BASE 0x0 A311D_GPIO_AO_LENGTH>,
				<0x0 A311D_GPIO_INTERRUPT_BASE 0x0 A311D_GPIO_INTERRUPT_LENGTH>;
			interrupts = <GIC_SPI 64 GIC_IRQ_MODE_EDGE_RISING>,
				<GIC_SPI 65 GIC_IRQ_MODE_EDGE_RISING>,
				<GIC_SPI 66 GIC_IRQ_MODE_EDGE_RISING>,
				<GIC_SPI 67 GIC_IRQ_MODE_EDGE_RISING>,
				<GIC_SPI 68 GIC_IRQ_MODE_EDGE_RISING>,
				<GIC_SPI 69 GIC_IRQ_MODE_EDGE_RISING>,
				<GIC_SPI 70 GIC_IRQ_MODE_EDGE_RISING>,
				<GIC_SPI 71 GIC_IRQ_MODE_EDGE_RISING>;
			gpio-controller;
			#gpio-cells = <2>;

			audio_tdm_a_cfg1: audio-tdm-a-cfg1 {
				pins = <A311D_GPIOX(11) A311D_GPIOX(10) A311D_GPIOX(9)>;
				function = <0 1>;
				drive-strength-microamp = <0 3000>;
			};

			audio_tdm_a_cfg2: audio-tdm-a-cfg2 {
				pins = <A311D_GPIOX(8)>;
				function = <0 2>;
			};

			audio_tdm_b_cfg1: audio-tdm-b-cfg1 {
				pins = <A311D_GPIOA(1) A311D_GPIOA(2) A311D_GPIOA(3)>;
				function = <0 1>;
				drive-strength-microamp = <0 3000>;
			};

			audio_tdm_b_cfg2: audio-tdm-b-cfg2 {
				pins = <A311D_GPIOA(4)>;
				function = <0 2>;
			};

			usb_power: usb-power {
				pins = <A311D_GPIOA(6)>;
				output-high;
			};

			emmc_d0_d7_cfg: emmc-d0-d7-cfg {
				pins = <A311D_GPIOBOOT(0) A311D_GPIOBOOT(1) A311D_GPIOBOOT(2) A311D_GPIOBOOT(3)>,
						<A311D_GPIOBOOT(4) A311D_GPIOBOOT(5) A311D_GPIOBOOT(6) A311D_GPIOBOOT(7)>;
				function = <0 1>;
			};

			emmc_clk_cfg: emmc-clk-cfg {
				pins = <A311D_GPIOBOOT(8)>;
				function = <0 1>;
			};

			emmc_cmd_cfg: emmc-cmd-cfg {
				pins = <A311D_GPIOBOOT(10)>;
				function = <0 1>;
			};

			emmc_ds_cfg: emmc-ds-cfg {
				pins = <A311D_GPIOBOOT(13)>;
				function = <0 1>;
			};

			emmc_cs_cfg: emmc-cs-cfg {
				pins = <A311D_GPIOBOOT(14)>;
				output-high;
			};

			sdcard_d0_d3_cfg: sdcard-d0-d3-cfg{
				pins = <A311D_GPIOC(0) A311D_GPIOC(1) A311D_GPIOC(2) A311D_GPIOC(3)>;
				function = <0 1>;
			};

			sdcard_clk_cfg: sdcard-clk-cfg{
				pins = <A311D_GPIOC(4)>;
				function = <0 1>;
			};

			sdcard_cmd_cfg: sdcard-cmd-cfg{
				pins = <A311D_GPIOC(5)>;
				function = <0 1>;
			};
		};

		audio: audio-controller@ff642000 {
			compatible = "amlogic,audio-controller";
			reg = <0x0 0xff642000 0x0 0x2000>;
			iommus = <&dummy_iommu 4>;
			clocks = <&clock 0x1000b &clock 0x20002>;
			clock-names = "AUDIO_GATE", "AUDIO_PLL" ;
			assigned-clocks = <&clock 0x20002>;
			assigned-clock-rates = <768000000>;
			gpios = <&gpio A311D_GPIOX(11) 0 &gpio A311D_GPIOA(1) 0>;
			gpio-names = "TDM_A_SCLK", "TDM_B_SCLK";
			pinctrl-0 = <&audio_tdm_a_cfg1 &audio_tdm_a_cfg2>,
						<&audio_tdm_b_cfg1 &audio_tdm_b_cfg2>;
		};

		aobus: bus@ff800000 {
			reg = <0x0 0xff800000 0x0 0x100000>;
			#address-cells = <2>;
			#size-cells = <2>;
			ranges = <0x0 0x0 0x0 0xff800000 0x0 0x100000>;

			i2c_AO: i2c@5000 {
				compatible = "amlogic,meson-axg-i2c";
				reg = <0x0 0x05000 0x0 0x20>;
				interrupts = <GIC_SPI 195 GIC_IRQ_MODE_EDGE_RISING>;
				#address-cells = <1>;
				#size-cells = <0>;
			};

			saradc: adc@9000 {
				compatible = "amlogic,meson-g12a-saradc";
				reg = <0x0 0x9000 0x0 0x1000>,
					<0x0 0x0 0x0 0x100000>;
				interrupts = <GIC_SPI 200 GIC_IRQ_MODE_EDGE_RISING>;
			};

		};

		gic: interrupt-controller@ffc01000 {
			compatible = "arm,gic-400";
			reg = <0x0 0xffc01000 0 0x1000>,
				  <0x0 0xffc02000 0 0x2000>,
				  <0x0 0xffc04000 0 0x2000>,
				  <0x0 0xffc06000 0 0x2000>;
			interrupt-controller;
			interrupts = <GIC_PPI 9 (GIC_CPU_MASK_SIMPLE(8) | GIC_IRQ_MODE_LEVEL_HIGH)>;
			#interrupt-cells = <3>;
			#address-cells = <0>;
		};

		cbus: bus@ffd00000 {
			reg = <0x0 0xFFD00000 0x0 0x100000>;
			#address-cells = <2>;
			#size-cells = <2>;
			ranges = <0x0 0x0 0x0 0xFFD00000 0x0 0x100000>;

			reset: register-controller@1000 {
				compatible = "fuchsia,registers";
				reg = <0x0 0x1000 0x0 0x100>;
				#register-cells = <4>;
			};

			mipi_dsi: dsi@7000 {
				compatible = "dw,dsi";
				reg = <0x0 0x7000 0x0 0x1000>;
			};

			i2c3: i2c@1c000 {
				compatible = "amlogic,meson-axg-i2c";
				reg = <0x0 0x1c000 0x0 0x20>;
				interrupts = <GIC_SPI 39 GIC_IRQ_MODE_EDGE_RISING>;
				#address-cells = <1>;
				#size-cells = <0>;
			};
		};

		usb_phy: phy@ffe09000 {
			compatible = "amlogic,g12b-usb-phy";
			reg = <0x0 A311D_USBCTRL_BASE 0x0 A311D_USBCTRL_LENGTH>,
				<0x0 A311D_USBPHY20_BASE 0x0 A311D_USBPHY20_LENGTH>,
				<0x0 A311D_USBPHY21_BASE 0x0 A311D_USBPHY21_LENGTH>,
				<0x0 A311D_USB3_PCIE_PHY_BASE 0x0 A311D_USB3_PCIE_PHY_LENGTH>;
			reg-names = "usb-ctrl", "usb2-phy", "usb2-otg-phy", "usb3-phy";
			dr_modes = "host", "peripheral", "host";
			interrupts = <GIC_SPI 16 GIC_IRQ_MODE_EDGE_RISING>;
			iommus = <&dummy_iommu 6>;
			assigned-clocks = <&clock 0x10008 &clock 0x10009 &clock 0x20001>;
			assigned-clock-rates = <0 0 100000000>;
			pinctrl-0 = <&usb_power>;
			registers = <&reset A311D_RESET1_REGISTER 4 0x0 0x10004>,
				<&reset A311D_RESET1_LEVEL 4 0x0 0x30000>;
			#phy-cells = <0>;
			pll-settings = <0x09400414 0x927E0000 0xac5f69e5 0xfe18>,
					<0x8000fff 0x78000 0xe0004 0xe000c>;
		};

		dwc2: usb@ff400000 {
			compatible = "snps,dwc2";
			reg = <0x0 A311D_USB1_BASE 0x0 A311D_USB1_LENGTH>;
			interrupts = <GIC_SPI 31 GIC_IRQ_MODE_EDGE_RISING>;
			iommus = <&dummy_iommu 6>;
			phys = <&usb_phy>;
			phy-names = "dwc2-phy";
			g-rx-fifo-size = <256>;
			g-np-tx-fifo-size = <32>;
			g-tx-fifo-size = <128 4 128 16>;
			g-turnaround-time = <9>;
			dma-burst-len = <5>;
			boot-metadata = <0x43414D6D 0>; /*DEVICE_METADATA_MAC_ADDRESS MACADDR_WIFI*/
		};

		xhci: usb@ff500000 {
			compatible = "generic-xhci";
			reg = <0x0 A311D_USB0_BASE 0x0 A311D_USB0_LENGTH>;
			interrupts = <GIC_SPI 30 GIC_IRQ_MODE_EDGE_RISING>;
			iommus = <&dummy_iommu 6>;
			phys = <&usb_phy>;
			phy-names = "xhci-phy";
		};

		sd_emmc_b: mmc@ffe05000 {
			compatible = "amlogic,meson-axg-mmc";
			reg = <0x0 0xffe05000 0x0 0x2000>;
			interrupts = <GIC_SPI 190 GIC_IRQ_MODE_EDGE_RISING>;
			iommus = <&dummy_iommu 7>;
			pinctrl-0 = <&sdcard_d0_d3_cfg &sdcard_clk_cfg &sdcard_cmd_cfg>;
			max-frequency = <50000000>;
		};

		sd_emmc_c: mmc@ffe07000 {
			compatible = "amlogic,meson-axg-mmc";
			reg = <0x0 0xffe07000 0x0 0x2000>;
			interrupts = <GIC_SPI 191 GIC_IRQ_MODE_EDGE_RISING>;
			iommus = <&dummy_iommu 6>;
			boot-metadata = <0x5452506D 0>;
			clocks = <&clock 0x1000C>;
			gpios = <&gpio A311D_GPIOBOOT(12) 0>;
			gpio-names = "SDMMC_RESET";
			pinctrl-0 = <&emmc_d0_d7_cfg &emmc_clk_cfg &emmc_cmd_cfg>,
						<&emmc_ds_cfg &emmc_cs_cfg>;
			max-frequency = <120000000>;
			non-removable;
			no-mmc-hs400;
		};

		aml_gpu: gpu@ffe40000 {
			compatible = "amlogic,gpu";
			reg = <0x0 0xffe40000 0x0 0x40000>, // MALI GPU
				<0x0 0xff63c000 0x0 0x2000>;   // HUI
			registers = <&reset A311D_RESET0_MASK 4 0x0 0x100000>,
					<&reset A311D_RESET0_LEVEL 4 0x0 0x100000>,
					<&reset A311D_RESET2_MASK 4 0x0 0x4000>,
					<&reset A311D_RESET2_LEVEL 4 0x0 0x4000>;
		};

		arm-mali@ffe40000 {
			compatible = "arm,mali-gpu";
			reg = <0x0 0xffe40000 0x0 0x40000>;
			interrupts = <GIC_SPI 162 GIC_IRQ_MODE_LEVEL_HIGH>,
				<GIC_SPI 161 GIC_IRQ_MODE_LEVEL_HIGH>,
				<GIC_SPI 160 GIC_IRQ_MODE_LEVEL_HIGH>;
			mali-gpu-parent = <&aml_gpu>;
			iommus = <&dummy_iommu 5>;
		};
	};

	nna@ff100000 {
		compatible = "amlogic,nna";
		reg = <0x0 A311D_NNA_BASE 0x0 A311D_NNA_LENGTH>,
				<0x0 A311D_HIU_BASE 0x0 A311D_HIU_LENGTH>,
				<0x0 A311D_POWER_DOMAIN_BASE 0x0 A311D_POWER_DOMAIN_LENGTH>,
				<0x0 A311D_MEMORY_PD_BASE 0x0 A311D_MEMORY_PD_LENGTH>,
				<0x0 A311D_NNA_SRAM_BASE  0x0 A311D_NNA_SRAM_LENGTH>;
		interrupts = <GIC_SPI 147 GIC_IRQ_MODE_LEVEL_HIGH>;
		iommus = <&dummy_iommu 3>;
		registers = <&reset A311D_RESET2_LEVEL 4 0x0 0x1000>;
	};
};
