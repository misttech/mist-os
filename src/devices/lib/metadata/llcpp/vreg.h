// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_LIB_METADATA_LLCPP_VREG_H_
#define SRC_DEVICES_LIB_METADATA_LLCPP_VREG_H_

#include <fidl/fuchsia.hardware.vreg/cpp/wire.h>

#include <vector>

namespace vreg {

using fuchsia_hardware_vreg::wire::PwmVregMetadata;
PwmVregMetadata BuildMetadata(fidl::AnyArena& allocator, uint32_t pwm_index, uint32_t period_ns,
                              uint32_t min_voltage_uv, uint32_t voltage_step_uv,
                              uint32_t num_steps) {
  PwmVregMetadata metadata(allocator);
  metadata.set_pwm_index(pwm_index);
  metadata.set_period_ns(period_ns);
  metadata.set_min_voltage_uv(min_voltage_uv);
  metadata.set_voltage_step_uv(voltage_step_uv);
  metadata.set_num_steps(num_steps);
  return metadata;
}

}  // namespace vreg

#endif  // SRC_DEVICES_LIB_METADATA_LLCPP_VREG_H_
