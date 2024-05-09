// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sysmem-test.h"

/dts-v1/;
/ {
		compatible = "fuchsia,test-dt";
		model = "test";
		#address-cells = <2>;
		#size-cells = <2>;

		sysmem: fuchsia,sysmem {
			compatible = "fuchsia,sysmem";
			pid = <TEST_PID>;
			vid = <TEST_VID>;
			size = <0x00 0x00>;
			contiguous-size = <0x00 TEST_CONTIGUOUS_SIZE>;
			protected-size = <0x00 TEST_PROTECTED_SIZE>;
		};

		vdec {
			compatible = "test,vdec";
			fuchsia,sysmem = <&sysmem>;
		};
};
