// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file

#include "gpio.h"

/dts-v1/;
/ {
    compatible = "fuchsia,test-dt";
    model = "test";
    #address-cells = <2>;
    #size-cells = <2>;

    gpioA: gpio-controller@ffffa000 {
      compatible = "fuchsia,gpio";
      reg = <0x0 0xffffa000 0x0 0x1000>;
      #gpio-cells = <2>;
      gpio-controller;

      init-HOG_PIN1 {
        gpio-hog;
        gpios = <HOG_PIN1 0>;
        output-low;
      };

      init-HOG_PIN2 {
        gpio-hog;
        gpios = <HOG_PIN2 HOG_PIN2_FLAG HOG_PIN3 HOG_PIN3_FLAG>;
        input;
      };
    };

    audio@0 {
      reg = <0x0 0x0 0x0 0x100>;
      compatible = "usb-peripheral";
      gpios = <&gpioA PIN1 0 &gpioA PIN2 0>;
      gpio-names = PIN1_NAME, PIN2_NAME;
    };

};
