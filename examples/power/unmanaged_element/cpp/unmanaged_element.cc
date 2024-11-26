// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/power/unmanaged_element/cpp/unmanaged_element.h"

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <lib/driver/power/cpp/element-description-builder.h>
#include <lib/driver/power/cpp/power-support.h>
#include <lib/driver/power/cpp/types.h>
#include <lib/fit/result.h>

#include <algorithm>
#include <optional>
#include <string>

#include "lib/fit/internal/result.h"

namespace examples::power {

fit::result<UnmanagedElement::Error, UnmanagedElement> UnmanagedElement::Add(
    fidl::ClientEnd<fuchsia_power_broker::Topology>& topology, const std::string& name,
    const std::vector<fuchsia_power_broker::PowerLevel>& valid_levels,
    fuchsia_power_broker::PowerLevel initial_current_level) {
  // Convert valid_levels from power_broker::PowerLevel to hardware_power::PowerLevel.
  std::vector<fdf_power::PowerLevel> hw_valid_levels(valid_levels.size());
  std::transform(valid_levels.begin(), valid_levels.end(), hw_valid_levels.begin(),
                 [](const fuchsia_power_broker::PowerLevel& broker_level) {
                   return fdf_power::PowerLevel{.level = broker_level};
                 });

  // Configure the ElementDescription for the new power element.
  fdf_power::PowerElementConfiguration config{
      .element = fdf_power::PowerElement{.name = name, .levels = std::move(hw_valid_levels)},
      .dependencies{}};
  fidl::Arena arena;
  fdf_power::ElementDescBuilder builder(config, fdf_power::TokenMap());

  // Create the unmanaged element and invalidate its dependency tokens.
  UnmanagedElement element(builder.Build());
  element.description_.assertive_token.reset();
  element.description_.opportunistic_token.reset();

  // Add the element to the Power Broker topology.
  fit::result<Error> add_result = fdf_power::AddElement(topology, element.description_);
  if (add_result.is_error()) {
    return add_result.take_error();
  }

  return fit::ok(std::move(element));
}

void UnmanagedElement::SetLevel(fuchsia_power_broker::PowerLevel target_level) {
  auto _result = fidl::WireCall(*description_.current_level_client)->Update(target_level);
}

}  // namespace examples::power
