// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/dts-v1/;

/include/ "meson-g12-common.dtsi"

#include <soc/aml-a311d/a311d-gpio.h>

/ {
	compatible = "amlogic,a311d";
	#address-cells = <2>;
	#size-cells = <2>;

	canvas: canvas@ff638000{
		compatible = "amlogic,canvas";
		reg = <0x0 0xff638000 0x0 0x2000>;
		iommus = <&dummy_iommu 2>;
	};

	clock: clock-controller@ff63c000 {
		compatible = "amlogic,g12b-clk";
		reg = <0x00 0xff63c000 0x00 0x2000>, <0x00 0xff620000 0x00 0x10000>, <0x00 0xffd18000 0x00 0x1000>;
		#clock-cells = <1>;
	};

	vddcpu0: pwm_a0_d-regulator {
		compatible = "amlogic,pwm-regulator";
		regulator-name = "pwm_vreg_little";
		pwms = <&pwm 9 1250 0>; // A311D_PWM_A0_D
		pinctrl-0 = <&pwm_a0_d_regulator_pins>;
		regulator-min-microvolt = <690000>;
		regulator-max-microvolt = <1050000>;
		regulator-step-microvolt = <1000>;
		#regulator-cells = <0>;
	 };

	vddcpu1: pwm_a-regulator {
		compatible = "amlogic,pwm-regulator";
		regulator-name = "pwm_vreg_big";
		pwms = <&pwm 0 1250 0>; // A311D_PWM_A
		pinctrl-0 = <&pwm_a_regulator_pins>;
		regulator-min-microvolt = <690000>;
		regulator-max-microvolt = <1050000>;
		regulator-step-microvolt = <1000>;
		#regulator-cells = <0>;
	};
};

&gpio {
	pwm_a0_d_regulator_pins: pwm-a0-d-regulator-pins {
		pins = <A311D_GPIOE(1)>;
		function = <0 A311D_GPIOE_1_PWM_D_FN>;
	};

	pwm_a_regulator_pins: pwm-a-regulator-pins {
		pins = <A311D_GPIOE(2)>;
		function = <0 A311D_GPIOE_2_PWM_D_FN>;
	};
};
