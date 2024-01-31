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
    };

    clkB: clock-controller@ffffb000 {
      compatible = "fuchsia,clock";
      reg = <0x0 0xffffb000 0x0 0x1000>;
      #clock-cells = <1>;
    };

    video {
      compatible = "test-video";
      clocks = <&clkA CLK_ID1 &clkA CLK_ID2>;
      clock-names = CLK1_NAME, CLK2_NAME;
    };

    audio {
      compatible = "test-audio";
      assigned-clocks = <&clkA CLK_ID3 &clkB CLK_ID4>;
      assigned-clock-rates = <0 CLK_ID4_RATE>;
      assigned-clock-parents = <&clkA CLK_ID5>;
    };

};
