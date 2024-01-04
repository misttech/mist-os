// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/dts-v1/;

/include/ "src/devices/board/drivers/vim3-devicetree/dts/amlogic-a311d.dtsi";

#include <lib/ddk/platform-defs.h>

/ {
	compatible = "khadas,vim3", "amlogic,a311d";
	model = "Khadas Vim3";

	dummy_iommu:iommu {
		/* This device has no hardware IOMMU. Use unique BTI id as iommu   */
		/* specifier for each consumer device for bookkeeping system BTIs. */
		#iommu-cells = <0x1>;
	};

	reserved-memory {
		#address-cells = <0x02>;
		#size-cells = <0x02>;
		ranges;

		fuchsia,contiguous {
			compatible = "sysmem";
			iommus = <&dummy_iommu 1>;
			pid = <PDEV_PID_AMLOGIC_A311D>;
			vid = <PDEV_VID_AMLOGIC>;
			// The AMlogic display engine needs contiguous physical memory for each
			// frame buffer, because it does not have a page table walker. We reserve
			// enough memory to hold 5 framebuffers (2 for virtcon, 3 for scenic) at
			// the maximum supported resolution of 3840 x 2160 with 4 bytes per pixel
			// (for the RGBA/BGRA 8888 pixel format).
			//
			// The maximum supported resolution is documented below.
			// * "A311D Quick Reference Manual" revision 01, pages 2-3
			// * "A311D Datasheet" revision 08, section 2.2 "Features", pages 4-5
			//
			// TODO(https://fxbug.dev/121456): Reserving this much memory seems wasteful. The
			// quantity below is 10% of RAM on a VIM3 Basic and 5% of RAM on a VIM3 Pro.
			size = <0x00 (200*1024*1024)>;
		};
	};

};
