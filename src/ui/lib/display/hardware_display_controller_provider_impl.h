// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_LIB_DISPLAY_HARDWARE_DISPLAY_CONTROLLER_PROVIDER_IMPL_H_
#define SRC_UI_LIB_DISPLAY_HARDWARE_DISPLAY_CONTROLLER_PROVIDER_IMPL_H_

// TODO(fxbug.dev/127211): Remove this file after migrating all clients to the
// headers in //src/graphics/display/testing.

#include "src/graphics/display/testing/coordinator-provider-lib/devfs-factory-hlcpp.h"

namespace ui_display {

using HardwareDisplayCoordinatorProviderImpl = display::DevFsCoordinatorFactoryHlcpp;

}  // namespace ui_display

#endif  // SRC_UI_LIB_DISPLAY_HARDWARE_DISPLAY_CONTROLLER_PROVIDER_IMPL_H_
