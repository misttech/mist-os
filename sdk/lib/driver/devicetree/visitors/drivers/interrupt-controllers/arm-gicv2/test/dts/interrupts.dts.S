// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file

/dts-v1/;

#include "interrupts.h"

/ {
    compatible = "fuchsia,test-dt";
    model = "test";
    #address-cells = <2>;
    #size-cells = <2>;
    interrupt-parent = <&gic>;

    gic: interrupt-controller@ffffd0000 {
        compatible = "arm,gic-400";
        #interrupt-cells = <3>;
        interrupt-controller;
        reg = <0x0 0xFFFFD000 0x0 0x1000>;
    };

    sample-device@1 {
        compatible = "fuchsia,sample-device";
        interrupts = <0 IRQ1_SPI IRQ1_MODE
                      1 IRQ2_PPI IRQ2_MODE>;
        reg = <0x0 0x1 0x0 0x1>;
    };

    sample-device@2 {
        compatible = "fuchsia,sample-device";
        interrupts-extended = <&gic 0 IRQ3_SPI IRQ3_MODE
                               &gic 1 IRQ4_PPI IRQ4_MODE>;
        reg = <0x0 0x2 0x0 0x1>;
    };
};
