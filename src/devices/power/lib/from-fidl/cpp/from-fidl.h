// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_POWER_LIB_FROM_FIDL_CPP_FROM_FIDL_H_
#define SRC_DEVICES_POWER_LIB_FROM_FIDL_CPP_FROM_FIDL_H_

#include <fidl/fuchsia.hardware.power/cpp/natural_types.h>

#include "sdk/lib/driver/power/cpp/types.h"

namespace power::from_fidl {

zx::result<fdf_power::Transition> CreateTransition(const fuchsia_hardware_power::Transition& src);

zx::result<fdf_power::LevelTuple> CreateLevelTuple(const fuchsia_hardware_power::LevelTuple& src);

zx::result<fdf_power::PowerLevel> CreatePowerLevel(const fuchsia_hardware_power::PowerLevel& src);

zx::result<fdf_power::PowerElement> CreatePowerElement(
    const fuchsia_hardware_power::PowerElement& src);

zx::result<fdf_power::ParentElement> CreateParentElement(
    const fuchsia_hardware_power::ParentElement& src);

zx::result<fdf_power::PowerDependency> CreatePowerDependency(
    const fuchsia_hardware_power::PowerDependency& src);

zx::result<fdf_power::PowerElementConfiguration> CreatePowerElementConfiguration(
    const fuchsia_hardware_power::PowerElementConfiguration& src);

}  // namespace power::from_fidl

#endif  // SRC_DEVICES_POWER_LIB_FROM_FIDL_CPP_FROM_FIDL_H_
