// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_ALPHA_MODE_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_ALPHA_MODE_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>

#include <cstdint>

namespace display {

// Equivalent to the FIDL type [`fuchsia.hardware.display/AlphaMode`] and the
// banjo type [`fuchsia.hardware.display.controller/Alpha`].
//
// See `::fuchsia_hardware_display::wire::AlphaMode` for references.
using AlphaMode = fuchsia_hardware_display::wire::AlphaMode;

AlphaMode ToAlphaMode(fuchsia_hardware_display::wire::AlphaMode alpha_mode_fidl);
AlphaMode ToAlphaMode(alpha_t alpha_banjo);

fuchsia_hardware_display::wire::AlphaMode ToFidlAlphaMode(AlphaMode alpha_mode);
alpha_t ToBanjoAlpha(AlphaMode alpha_mode);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_ALPHA_MODE_H_
