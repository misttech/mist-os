// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file

/dts-v1/;

#include "reg.h"

/ {
    compatible = "fuchsia,test-dt";
    model = "test";
    #address-cells = <2>;
    #size-cells = <2>;

    sample-device@REG_UNIT_NAME {
        compatible = "fuchsia,test-device";
        reg = <0 REG_A_BASE 0 REG_A_LENGTH
               REG_B_BASE_WORD0 REG_B_BASE_WORD1
               REG_B_LENGTH_WORD0 REG_B_LENGTH_WORD1
               REG_C_BASE_WORD0 REG_C_BASE_WORD1
               REG_C_LENGTH_WORD0 REG_C_LENGTH_WORD1>;
    };
};
