// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/lib/perfmon/properties_impl.h"

namespace perfmon::internal {

void FidlToPerfmonProperties(const fuchsia_perfmon_cpu::Properties& props, Properties* out_props) {
  *out_props = {};

  out_props->api_version = props.api_version();
  out_props->pm_version = props.pm_version();

  out_props->max_num_events = props.max_num_events();

  out_props->max_num_fixed_events = props.max_num_fixed_events();
  out_props->max_fixed_counter_width = props.max_fixed_counter_width();

  out_props->max_num_programmable_events = props.max_num_programmable_events();
  out_props->max_programmable_counter_width = props.max_programmable_counter_width();

  out_props->max_num_misc_events = props.max_num_misc_events();
  out_props->max_misc_counter_width = props.max_misc_counter_width();

  out_props->flags = 0;
  if ((props.flags() & fuchsia_perfmon_cpu::PropertyFlags::kHasLastBranch) ==
      fuchsia_perfmon_cpu::PropertyFlags::kHasLastBranch) {
    out_props->flags |= Properties::kFlagHasLastBranch;
  }
}

}  // namespace perfmon::internal
