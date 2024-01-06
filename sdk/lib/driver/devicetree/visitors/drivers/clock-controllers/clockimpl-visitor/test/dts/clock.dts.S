// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file

#include "clock.h"

/dts-v1/;
/ {
    compatible = "fuchsia,test-dt";
    model = "test";
    #address-cells = <2>;
    #size-cells = <2>;

    clkA: clock-controller@ffffa000 {
      compatible = "fuchsia,clock";
      reg = <0x0 0xffffa000 0x0 0x1000>;
      #clock-cells = <1>;
      phandle = <TEST_PHANDLE>;
    };

    video@0 {
      reg = <0x0 0x0 0x0 0x100>;
      compatible = "test-video";
      clocks = <&clkA CLK_ID1 &clkA CLK_ID2>;
      clock-names = CLK1_NAME, CLK2_NAME;
    };

};
