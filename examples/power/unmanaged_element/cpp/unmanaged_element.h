// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef EXAMPLES_POWER_UNMANAGED_ELEMENT_CPP_UNMANAGED_ELEMENT_H_
#define EXAMPLES_POWER_UNMANAGED_ELEMENT_CPP_UNMANAGED_ELEMENT_H_

#include <fidl/fuchsia.power.broker/cpp/fidl.h>
#include <lib/driver/power/cpp/power-support.h>
#include <lib/fidl/cpp/client.h>
#include <lib/fit/result.h>

#include <string>

namespace examples::power {

class UnmanagedElement {
 public:
  using Error = fdf_power::Error;

  // Synchronously creates and adds an UnmanagedElement to the Power Broker Topology.
  static fit::result<Error, UnmanagedElement> Add(
      fidl::ClientEnd<fuchsia_power_broker::Topology>& topology, const std::string& name,
      const std::vector<fuchsia_power_broker::PowerLevel>& valid_levels,
      fuchsia_power_broker::PowerLevel initial_current_level);

  // Synchronously updates the current power level of this unmanaged element in the Topology.
  void SetLevel(fuchsia_power_broker::PowerLevel target_level);

  const fdf_power::ElementDesc& description() const { return description_; }

  // Destroying this UnmanagedElement will close all associated FIDL connections with Power Broker.
  ~UnmanagedElement() = default;

  // Move constructor and assignment operator are required for underlying implementation.
  UnmanagedElement(UnmanagedElement&&) = default;
  UnmanagedElement& operator=(UnmanagedElement&&) = default;

 private:
  explicit UnmanagedElement(fdf_power::ElementDesc description)
      : description_(std::move(description)) {}

  // Holds ClientEnds for CurrentLevel, RequiredLevel, Lessor, ElementControl. When this object is
  // destroyed, the corresponding Power Broker element will be destroyed on the server.
  fdf_power::ElementDesc description_;
};

}  // namespace examples::power

#endif  // EXAMPLES_POWER_UNMANAGED_ELEMENT_CPP_UNMANAGED_ELEMENT_H_
