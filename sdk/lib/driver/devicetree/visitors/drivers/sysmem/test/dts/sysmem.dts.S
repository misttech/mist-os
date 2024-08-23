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

		dummy_iommu:iommu {
			#iommu-cells = <1>;
		};

		sysmem: fuchsia,sysmem {
			compatible = "fuchsia,sysmem";
			size = <0x00 0x00>;
			iommus = <&dummy_iommu TEST_BTI_ID>;
		};
};
