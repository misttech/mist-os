// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_PREFERRED_SCANOUT_IMAGE_TYPE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_PREFERRED_SCANOUT_IMAGE_TYPE_H_

#include <cstdint>

// display-controller.fidl states that TYPE_SIMPLE is the only universally-supported image type
// defined by the API, and that any other value must be agreed upon by all parties (e.g. the image
// producer, the display driver, etc.) through some other means, perhaps a future negotiation API.
// For now, this header serves the role of "some other means".

#if defined(__x86_64__)
// `fuchsia.hardware.intelgpucore/IMAGE_TYPE_X_TILED` from the Banjo API.
constexpr uint32_t IMAGE_TYPE_PREFERRED_SCANOUT = 1;
#elif defined(__aarch64__)
// `fuchsia.hardware.display/TYPE_SIMPLE` from the FIDL API.
// `fuchsia.hardware.display.controller/ImageType.SIMPLE` from the Banjo API.
constexpr uint32_t IMAGE_TYPE_PREFERRED_SCANOUT = 0;
#elif defined(__riscv)
// `fuchsia.hardware.display/TYPE_SIMPLE` from the FIDL API.
// `fuchsia.hardware.display.controller/ImageType.SIMPLE` from the Banjo API.
//
// This may be revisited, depending on the hardware that we end supporting.
constexpr uint32_t IMAGE_TYPE_PREFERRED_SCANOUT = 0;
#else
#error "Preferred scanout image format not defined for this platform."
#endif

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_PREFERRED_SCANOUT_IMAGE_TYPE_H_
