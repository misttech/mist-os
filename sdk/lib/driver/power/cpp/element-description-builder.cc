// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/driver/power/cpp/element-description-builder.h"

#include <fidl/fuchsia.hardware.power/cpp/fidl.h>
#include <lib/zx/event.h>

namespace fdf_power {

ElementDesc ElementDescBuilder::Build() {
  ElementDesc to_return;
  to_return.element_config_ = element_config_;
  to_return.tokens_ = std::move(tokens_);

  if (this->active_token_.has_value()) {
    to_return.active_token_ = std::move(this->active_token_.value());
  } else {
    // make an event instead
    zx::event::create(0, &to_return.active_token_);
  }

  if (this->passive_token_.has_value()) {
    to_return.passive_token_ = std::move(this->passive_token_.value());
  } else {
    // make an event instead
    zx::event::create(0, &to_return.passive_token_);
  }

  fidl::ServerEnd<fuchsia_power_broker::RequiredLevel> required_level_server;
  std::optional<fidl::ClientEnd<fuchsia_power_broker::RequiredLevel>> required_level_client;

  if (this->required_level_.has_value()) {
    required_level_server = std::move(this->required_level_.value());
  } else {
    // make a channel instead, include it in output
    fidl::Endpoints<fuchsia_power_broker::RequiredLevel> endpoints =
        fidl::CreateEndpoints<fuchsia_power_broker::RequiredLevel>().value();
    required_level_server = std::move(endpoints.server);
    required_level_client = std::move(endpoints.client);
  }

  fidl::ServerEnd<fuchsia_power_broker::CurrentLevel> current_level_server;
  std::optional<fidl::ClientEnd<fuchsia_power_broker::CurrentLevel>> current_level_client;
  if (this->current_level_.has_value()) {
    current_level_server = std::move(this->current_level_.value());
  } else {
    // make a channel instead, include it in output
    fidl::Endpoints<fuchsia_power_broker::CurrentLevel> endpoints =
        fidl::CreateEndpoints<fuchsia_power_broker::CurrentLevel>().value();
    current_level_server = std::move(endpoints.server);
    current_level_client = std::move(endpoints.client);
  }

  to_return.level_control_servers_ =
      std::make_pair(std::move(current_level_server), std::move(required_level_server));
  to_return.current_level_client_ = std::move(current_level_client);
  to_return.required_level_client_ = std::move(required_level_client);

  if (this->lessor_.has_value()) {
    to_return.lessor_server_ = std::move(this->lessor_.value());
  } else {
    // make a channel instead, include it in output
    fidl::Endpoints<fuchsia_power_broker::Lessor> endpoints =
        fidl::CreateEndpoints<fuchsia_power_broker::Lessor>().value();
    to_return.lessor_client_ = std::move(endpoints.client);
    to_return.lessor_server_ = std::move(endpoints.server);
  }

  return to_return;
}

ElementDescBuilder& ElementDescBuilder::SetActiveToken(const zx::unowned_event& active_token) {
  zx::event dupe;
  active_token->duplicate(ZX_RIGHT_SAME_RIGHTS, &dupe);
  active_token_ = std::move(dupe);
  return *this;
}

ElementDescBuilder& ElementDescBuilder::SetPassiveToken(const zx::unowned_event& passive_token) {
  zx::event dupe;
  passive_token->duplicate(ZX_RIGHT_SAME_RIGHTS, &dupe);
  passive_token_ = std::move(dupe);
  return *this;
}

ElementDescBuilder& ElementDescBuilder::SetCurrentLevel(
    fidl::ServerEnd<fuchsia_power_broker::CurrentLevel> current) {
  current_level_ = std::move(current);
  return *this;
}

ElementDescBuilder& ElementDescBuilder::SetRequiredLevel(
    fidl::ServerEnd<fuchsia_power_broker::RequiredLevel> required) {
  required_level_ = std::move(required);
  return *this;
}

ElementDescBuilder& ElementDescBuilder::SetLessor(
    fidl::ServerEnd<fuchsia_power_broker::Lessor> lessor) {
  lessor_ = std::move(lessor);
  return *this;
}

}  // namespace fdf_power
