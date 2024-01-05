// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file

#include "i2c.h"

/dts-v1/;
/ {
    compatible = "fuchsia,test-dt";
    model = "test";
    #address-cells = <2>;
    #size-cells = <2>;

    i2c@ffffa000 {
      compatible = "fuchsia,i2c";
      reg = <0x0 0xffffa000 0x0 0x1000>;
      #address-cells = <1>;
      #size-cells = <0>;

      child@c {
        compatible = "fuchsia,test";
        reg = <I2C_ADDRESS1>;
      };

      child@1e {
        compatible = "fuchsia,test";
        reg = <I2C_ADDRESS2>;
      };
    };
};
