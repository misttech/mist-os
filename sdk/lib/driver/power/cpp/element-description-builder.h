// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_POWER_CPP_ELEMENT_DESCRIPTION_BUILDER_H_
#define LIB_DRIVER_POWER_CPP_ELEMENT_DESCRIPTION_BUILDER_H_

#include <fidl/fuchsia.hardware.power/cpp/fidl.h>

#include "sdk/lib/driver/power/cpp/power-support.h"

namespace fdf_power {
class ElementDescBuilder {
 public:
  explicit ElementDescBuilder(fuchsia_hardware_power::wire::PowerElementConfiguration config,
                              TokenMap tokens)
      : element_config_(config), tokens_(std::move(tokens)) {}

  /// Build an `ElementDesc` object based on the information we've been given.
  ///
  /// If assertive or opportunistic tokens are not set, `zx::event` objects are
  /// created.
  ///
  /// If current level, required level, or lessor channels are not set, these
  /// are created.  The `fidl::ClientEnd` of these channel is placed in the
  /// `current_level_client_`, `required_level_client_`, and `lessor_client_`
  /// fields of the `ElementDesc` object returned.
  ElementDesc Build();

  /// Sets the assertive token to associate with this element by duplicating
  /// the token passed in.
  ElementDescBuilder& SetAssertiveToken(const zx::unowned_event& assertive_token);

  /// Sets the opportunistic token to associate with this element by duplicating the
  /// token passed in.
  ElementDescBuilder& SetOpportunisticToken(const zx::unowned_event& opportunistic_token);

  /// Sets the channel to use for the CurrentLevel protocol.
  ElementDescBuilder& SetCurrentLevel(fidl::ServerEnd<fuchsia_power_broker::CurrentLevel> current);

  /// Sets the channel to use for the RequiredLevel protocol.
  ElementDescBuilder& SetRequiredLevel(
      fidl::ServerEnd<fuchsia_power_broker::RequiredLevel> required);

  /// Sets the channel to use for the Lessor protocol.
  ElementDescBuilder& SetLessor(fidl::ServerEnd<fuchsia_power_broker::Lessor> lessor);

 private:
  fuchsia_hardware_power::wire::PowerElementConfiguration element_config_;
  TokenMap tokens_;
  std::optional<zx::event> assertive_token_;
  std::optional<zx::event> opportunistic_token_;
  std::optional<fidl::ServerEnd<fuchsia_power_broker::CurrentLevel>> current_level_;
  std::optional<fidl::ServerEnd<fuchsia_power_broker::RequiredLevel>> required_level_;
  std::optional<fidl::ServerEnd<fuchsia_power_broker::Lessor>> lessor_;
};

}  // namespace fdf_power

#endif  // LIB_DRIVER_POWER_CPP_ELEMENT_DESCRIPTION_BUILDER_H_
