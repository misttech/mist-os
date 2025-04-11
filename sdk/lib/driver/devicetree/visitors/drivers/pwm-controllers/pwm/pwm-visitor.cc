// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pwm-visitor.h"

#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/common-types.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>

#include <cstdint>
#include <regex>
#include <vector>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/hardware/pwm/cpp/bind.h>
#include <bind/fuchsia/pwm/cpp/bind.h>

namespace {
using fuchsia_hardware_pwm::PwmChannelInfo;
}

namespace pwm_visitor_dt {

PwmVisitor::PwmVisitor() {
  fdf_devicetree::Properties pwm_properties = {};
  pwm_properties.emplace_back(
      std::make_unique<fdf_devicetree::ReferenceProperty>(kPwmReference, kPwmCells));
  pwm_properties.emplace_back(std::make_unique<fdf_devicetree::StringListProperty>(kPwmNames));
  parser_ = std::make_unique<fdf_devicetree::PropertyParser>(std::move(pwm_properties));
}

bool PwmVisitor::is_match(const std::string& name) {
  std::regex name_regex("^pwm@[0-9a-f]+$");
  return std::regex_match(name, name_regex);
}

zx::result<> PwmVisitor::Visit(fdf_devicetree::Node& node,
                               const devicetree::PropertyDecoder& decoder) {
  zx::result parser_output = parser_->Parse(node);
  if (parser_output.is_error()) {
    FDF_LOG(ERROR, "PWM visitor parse failed for node '%s' : %s", node.name().c_str(),
            parser_output.status_string());
    return parser_output.take_error();
  }

  if (!parser_output->contains(kPwmReference)) {
    return zx::ok();
  }

  if (!parser_output->contains(kPwmNames) && (*parser_output)[kPwmReference].size() != 1u) {
    FDF_LOG(
        ERROR,
        "PWM reference '%s' does not have valid pwm names property. Name is required to generate bind rules, especially when more than one pwm is referenced.",
        node.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  const size_t reference_count = (*parser_output)[kPwmReference].size();
  std::vector<std::optional<std::string>> pwm_names(reference_count);
  if (parser_output->find(kPwmNames) != parser_output->end()) {
    if (parser_output->at(kPwmNames).size() != reference_count) {
      FDF_LOG(
          ERROR,
          "PWM reference '%s' does not expected number of pwm names property. Expected: %lu actual: %lu.",
          node.name().c_str(), reference_count, parser_output->at(kPwmNames).size());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    size_t name_idx = 0;
    for (auto& names : (*parser_output)[kPwmNames]) {
      pwm_names[name_idx++] = names.AsString();
    }
  }

  for (uint32_t index = 0; index < reference_count; index++) {
    auto reference = (*parser_output)[kPwmReference][index].AsReference();
    if (reference && is_match(reference->first.name())) {
      auto result =
          ParseReferenceChild(node, reference->first, reference->second, pwm_names[index]);
      if (result.is_error()) {
        return result.take_error();
      }
    }
  }

  return zx::ok();
}

PwmVisitor::PwmController& PwmVisitor::GetController(fdf_devicetree::Phandle phandle) {
  const auto [controller_iter, success] = pwm_controllers_.insert({phandle, PwmController()});
  return controller_iter->second;
}

zx::result<> PwmVisitor::ParseReferenceChild(fdf_devicetree::Node& child,
                                             fdf_devicetree::ReferenceNode& parent,
                                             fdf_devicetree::PropertyCells specifiers,
                                             std::optional<std::string_view> pwm_name) {
  auto& controller = GetController(*parent.phandle());

  if (specifiers.size_bytes() < 1 * sizeof(uint32_t)) {
    FDF_LOG(
        ERROR,
        "PWM reference '%s' has incorrect number of pwm specifiers (%lu) - expected at least 1.",
        child.name().c_str(), specifiers.size_bytes() / sizeof(uint32_t));
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  auto cells = fdf_devicetree::Uint32Array(specifiers);
  PwmChannelInfo pwm_channel = {};
  pwm_channel.id() = cells[0];

  // Second cell if present specifies the PWM period in nanoseconds.
  if (cells.size() >= 2) {
    pwm_channel.period_ns() = cells[1];
  }

  // Third cell if present would contain the PWM flags.
  if (cells.size() == 3) {
    if (cells[2] & static_cast<uint32_t>(PwmFlags::PWM_POLARITY_INVERTED)) {
      pwm_channel.polarity() = true;
    }
    if (cells[2] & static_cast<uint32_t>(PwmFlags::PWM_SKIP_INIT)) {
      pwm_channel.skip_init() = true;
    }
  }

  FDF_LOG(
      DEBUG,
      "PWM channel added - ID 0x%x, Period %d, Polarity %d, Skip init %d, and name '%s' to controller '%s'",
      *pwm_channel.id(), pwm_channel.period_ns().value_or(0), pwm_channel.polarity().value_or(0),
      pwm_channel.skip_init().value_or(0),
      pwm_name ? std::string(*pwm_name).c_str() : "<anonymous>", parent.name().c_str());

  if (!controller.pwm_channels.channels()) {
    controller.pwm_channels.channels() = std::vector<PwmChannelInfo>();
  }
  controller.pwm_channels.channels()->emplace_back(pwm_channel);

  return AddChildNodeSpec(child, cells[0], pwm_name);
}

zx::result<> PwmVisitor::AddChildNodeSpec(fdf_devicetree::Node& child, uint32_t id,
                                          std::optional<std::string_view> pwm_name) {
  auto pwm_node = fuchsia_driver_framework::ParentSpec2{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_pwm::SERVICE,
                                       bind_fuchsia_hardware_pwm::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeAcceptBindRule2(bind_fuchsia::PWM_ID, id),
          },
      .properties =
          {
              fdf::MakeProperty2(bind_fuchsia_hardware_pwm::SERVICE,
                                 bind_fuchsia_hardware_pwm::SERVICE_ZIRCONTRANSPORT),
          },
  }};

  if (pwm_name) {
    pwm_node.properties().push_back(
        fdf::MakeProperty2(bind_fuchsia_pwm::PWM_ID_FUNCTION,
                           "fuchsia.pwm.PWM_ID_FUNCTION." + std::string(*pwm_name)));
  }

  child.AddNodeSpec(pwm_node);
  return zx::ok();
}

zx::result<> PwmVisitor::FinalizeNode(fdf_devicetree::Node& node) {
  // Check that it is indeed a pwm that we support.
  if (!is_match(node.name())) {
    return zx::ok();
  }

  if (node.phandle()) {
    auto controller = pwm_controllers_.find(*node.phandle());
    if (controller == pwm_controllers_.end()) {
      FDF_LOG(INFO, "PWM controller '%s' is not being used. Not adding any metadata for it.",
              node.name().c_str());
      return zx::ok();
    }

    if (controller->second.pwm_channels.channels()) {
      fit::result encoded_metadata = fidl::Persist(controller->second.pwm_channels);
      if (encoded_metadata.is_error()) {
        FDF_LOG(ERROR, "Failed to encode pwm channels metadata: %s",
                encoded_metadata.error_value().FormatDescription().c_str());
        return zx::error(encoded_metadata.error_value().status());
      }

      fuchsia_hardware_platform_bus::Metadata channels_metadata = {{
          .id = std::to_string(DEVICE_METADATA_PWM_CHANNELS),
          .data = encoded_metadata.value(),
      }};

      node.AddMetadata(std::move(channels_metadata));
      FDF_LOG(DEBUG, "PWM Channels metadata added to node '%s'", node.name().c_str());
    }
  }
  return zx::ok();
}

}  // namespace pwm_visitor_dt

REGISTER_DEVICETREE_VISITOR(pwm_visitor_dt::PwmVisitor);
