// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/dts-v1/;

#include "src/devices/lib/amlogic/include/soc/aml-a311d/a311d-hw.h"
#include "sdk/lib/driver/devicetree/visitors/drivers/interrupt-controllers/arm-gicv2/arm-gicv2.h"

/ {
	interrupt-parent = <&gic>;

	soc {
		#address-cells = <2>;
		#size-cells = <2>;
		ranges;

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
	};

	nna@ff100000 {
		compatible = "amlogic,nna";
		reg = <0x0 A311D_NNA_BASE 0x0 A311D_NNA_LENGTH>,
				<0x0 A311D_HIU_BASE 0x0 A311D_HIU_LENGTH>,
				<0x0 A311D_POWER_DOMAIN_BASE 0x0 A311D_POWER_DOMAIN_LENGTH>,
				<0x0 A311D_MEMORY_PD_BASE 0x0 A311D_MEMORY_PD_LENGTH>,
				<0x0 A311D_NNA_SRAM_BASE  0x0 A311D_NNA_SRAM_LENGTH>;
		interrupts = <GIC_SPI 147 GIC_IRQ_MODE_LEVEL_HIGH>;
		iommus = <&dummy_iommu 2>;
		registers = <&reset A311D_RESET2_LEVEL 4 0x0 0x1000>;
	};
};
