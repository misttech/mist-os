// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_POWER_CPP_ELEMENT_DESCRIPTION_BUILDER_H_
#define LIB_DRIVER_POWER_CPP_ELEMENT_DESCRIPTION_BUILDER_H_

#include <fidl/fuchsia.hardware.platform.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <fidl/fuchsia.power.broker/cpp/fidl.h>

namespace fdf_power {
class ParentElementHasher final {
 public:
  /// Make a unique string as our hash key.
  size_t operator()(const fuchsia_hardware_power::ParentElement& element) const;
};

using TokenMap =
    std::unordered_map<fuchsia_hardware_power::ParentElement, zx::event, ParentElementHasher>;

using ElementDependencyMap =
    std::unordered_map<fuchsia_hardware_power::ParentElement,
                       std::vector<fuchsia_power_broker::LevelDependency>, ParentElementHasher>;

struct ElementDesc {
  fuchsia_hardware_power::wire::PowerElementConfiguration element_config_;
  TokenMap tokens;
  zx::event assertive_token;
  zx::event opportunistic_token;
  std::pair<fidl::ServerEnd<fuchsia_power_broker::CurrentLevel>,
            fidl::ServerEnd<fuchsia_power_broker::RequiredLevel>>
      level_control_servers;
  fidl::ServerEnd<fuchsia_power_broker::Lessor> lessor_server;
  fidl::ServerEnd<fuchsia_power_broker::ElementControl> element_control_server;

  // The below are created if the caller did not supply their corresponding server end
  std::optional<fidl::ClientEnd<fuchsia_power_broker::CurrentLevel>> current_level_client;
  std::optional<fidl::ClientEnd<fuchsia_power_broker::RequiredLevel>> required_level_client;
  std::optional<fidl::ClientEnd<fuchsia_power_broker::Lessor>> lessor_client;
  std::optional<fidl::ClientEnd<fuchsia_power_broker::ElementControl>> element_control_client;
};

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

  /// Sets the channel to use for the ElementControl protocol.
  ElementDescBuilder& SetElementControl(
      fidl::ServerEnd<fuchsia_power_broker::ElementControl> element_control);

 private:
  fuchsia_hardware_power::wire::PowerElementConfiguration element_config_;
  TokenMap tokens_;
  std::optional<zx::event> assertive_token_;
  std::optional<zx::event> opportunistic_token_;
  std::optional<fidl::ServerEnd<fuchsia_power_broker::CurrentLevel>> current_level_;
  std::optional<fidl::ServerEnd<fuchsia_power_broker::RequiredLevel>> required_level_;
  std::optional<fidl::ServerEnd<fuchsia_power_broker::Lessor>> lessor_;
  std::optional<fidl::ServerEnd<fuchsia_power_broker::ElementControl>> element_control_;
};

}  // namespace fdf_power

#endif  // LIB_DRIVER_POWER_CPP_ELEMENT_DESCRIPTION_BUILDER_H_
