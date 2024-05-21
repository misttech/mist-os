// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "power-domain-test.h"

/dts-v1/;
/ {
    compatible = "fuchsia,test-dt";
    model = "test";
    #address-cells = <2>;
    #size-cells = <2>;

    pwrc: power-controller {
      compatible = "test,power";
      #power-domain-cells = <1>;
    };

    cpufreq: cpufreq {
      compatible = "test,cpufreq";
      power-domains = <&pwrc TEST_DOMAIN_ID>;
    };
};
