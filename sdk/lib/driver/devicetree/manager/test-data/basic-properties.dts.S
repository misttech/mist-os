// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file

/dts-v1/;

#include "sdk/lib/driver/devicetree/manager/test-data/basic-properties.h"
#include <lib/ddk/platform-defs.h>

/ {
    compatible = "fuchsia,sample-dt";
    #address-cells = <2>;
    #size-cells = <2>;

    sample-device@0 {
        compatible = "fuchsia,sample-device";
        reg = <0 TEST_REG_A_BASE 0 TEST_REG_A_LENGTH>;
        device_specific_prop = <DEVICE_SPECIFIC_PROP_VALUE>;
    };

    sample_parent:reference-parent {
        compatible = "fuchsia,reference-parent";
        #property1-cells = <PROPERTY1_CELLS>;
        #property2-cells = <PROPERTY2_CELLS>;
    };

    sample-property1-child {
        compatible = "fuchsia,property1-child";
        property1 = <&sample_parent PROPERTY1_SPECIFIER>;
    };

    property2-child {
        compatible = "fuchsia,property2-child";
        property2 = <&sample_parent>;
    };

    parent-device {
        compatible = "fuchsia,parent";
        child1 {
            compatible = "fuchsia,child";
        };
        child2 {
            compatible = "fuchsia,child";
        };
    };
};
