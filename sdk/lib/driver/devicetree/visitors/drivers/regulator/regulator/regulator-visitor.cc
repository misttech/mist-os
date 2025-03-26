// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "regulator-visitor.h"

#include <fidl/fuchsia.hardware.vreg/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>
#include <zircon/errors.h>

#include <memory>
#include <regex>

#include <bind/fuchsia/hardware/vreg/cpp/bind.h>
#include <bind/fuchsia/regulator/cpp/bind.h>

namespace regulator_visitor_dt {

RegulatorVisitor::RegulatorVisitor() {
  fdf_devicetree::Properties reference_properties = {};
  reference_properties.emplace_back(
      std::make_unique<fdf_devicetree::ReferenceProperty>(kRegulatorReference, kRegulatorCells));
  reference_parser_ =
      std::make_unique<fdf_devicetree::PropertyParser>(std::move(reference_properties));

  fdf_devicetree::Properties regulator_properties = {};
  regulator_properties.emplace_back(
      std::make_unique<fdf_devicetree::StringListProperty>(kRegulatorName));
  regulator_properties.emplace_back(
      std::make_unique<fdf_devicetree::Uint32Property>(kRegulatorMaxMicrovolt));
  regulator_properties.emplace_back(
      std::make_unique<fdf_devicetree::Uint32Property>(kRegulatorMinMicrovolt));
  regulator_properties.emplace_back(
      std::make_unique<fdf_devicetree::Uint32Property>(kRegulatorStepMicrovolt));
  parser_ = std::make_unique<fdf_devicetree::PropertyParser>(std::move(regulator_properties));
}

bool RegulatorVisitor::is_match(const std::string& name) {
  std::regex name_regex(".*regulator(@.*)?$");
  return std::regex_match(name, name_regex);
}

zx::result<> RegulatorVisitor::Visit(fdf_devicetree::Node& node,
                                     const devicetree::PropertyDecoder& decoder) {
  if (is_match(node.name())) {
    auto parser_output = parser_->Parse(node);
    if (parser_output.is_error()) {
      FDF_LOG(ERROR, "Regulator visitor failed for node '%s' : %s", node.name().c_str(),
              parser_output.status_string());
      return parser_output.take_error();
    }

    auto status = AddRegulatorMetadata(node, parser_output.value());
    if (status.is_error()) {
      FDF_LOG(ERROR, "Failed to add regulator metadata '%s' : %s", node.name().c_str(),
              status.status_string());
      return status.take_error();
    }
  }

  auto reference_output = reference_parser_->Parse(node);
  if (reference_output.is_error()) {
    FDF_LOG(ERROR, "Regulator visitor failed for node '%s' : %s", node.name().c_str(),
            reference_output.status_string());
    return reference_output.take_error();
  }

  if (reference_output->empty()) {
    return zx::ok();
  }

  for (auto& reference : reference_output->at(kRegulatorReference)) {
    auto reference_node = reference.AsReference()->first;
    auto status = AddChildNodeSpec(node, reference_node);
    if (status.is_error()) {
      FDF_LOG(ERROR, "Failed to add regulator '%s' node spec to '%s' : %s",
              reference_node.name().c_str(), node.name().c_str(), status.status_string());
      return status.take_error();
    }
  }

  return zx::ok();
}

zx::result<> RegulatorVisitor::AddRegulatorMetadata(fdf_devicetree::Node& node,
                                                    fdf_devicetree::PropertyValues& values) {
  if (!values.contains(kRegulatorName)) {
    FDF_LOG(ERROR, "Regulator node '%s' does not have a name.", node.name().c_str());
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  fuchsia_hardware_vreg::VregMetadata metadata;
  metadata.name() = values.at(kRegulatorName)[0].AsString();

  if (values.find(kRegulatorMinMicrovolt) != values.end()) {
    metadata.min_voltage_uv() = values.at(kRegulatorMinMicrovolt)[0].AsUint32();
  }

  if (values.find(kRegulatorStepMicrovolt) != values.end()) {
    metadata.voltage_step_uv() = values.at(kRegulatorStepMicrovolt)[0].AsUint32();
  }

  if (values.find(kRegulatorMaxMicrovolt) != values.end() && metadata.voltage_step_uv() &&
      metadata.min_voltage_uv()) {
    if (values.at(kRegulatorMaxMicrovolt)[0].AsUint32().value() < *metadata.min_voltage_uv()) {
      FDF_LOG(ERROR, "Regulator max voltage (%d) is not more than min voltage (%d) in node '%s'",
              values.at(kRegulatorMaxMicrovolt)[0].AsUint32().value(), *metadata.min_voltage_uv(),
              node.name().c_str());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    auto voltage_range =
        (values.at(kRegulatorMaxMicrovolt)[0].AsUint32().value() - *metadata.min_voltage_uv());

    if (voltage_range % (*metadata.voltage_step_uv()) != 0) {
      FDF_LOG(ERROR, "Voltage range (%d) is not a multiple of step size (%d) for node '%s'",
              voltage_range, (*metadata.voltage_step_uv()), node.name().c_str());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    metadata.num_steps() = (voltage_range / *metadata.voltage_step_uv()) + 1;
  }

  const fit::result encoded_metadata = fidl::Persist(metadata);
  if (!encoded_metadata.is_ok()) {
    FDF_LOG(ERROR, "Failed to encode Vreg metadata for node %s: %s", node.name().c_str(),
            encoded_metadata.error_value().FormatDescription().c_str());
    return zx::error(encoded_metadata.error_value().status());
  }
  fuchsia_hardware_platform_bus::Metadata vreg_metadata = {{
      .id = std::to_string(DEVICE_METADATA_VREG),
      .data = encoded_metadata.value(),
  }};

  node.AddMetadata(std::move(vreg_metadata));
  FDF_LOG(DEBUG, "Added regulator metadata to node '%s'.", node.name().c_str());

  return zx::ok();
}

zx::result<> RegulatorVisitor::AddChildNodeSpec(fdf_devicetree::Node& child,
                                                fdf_devicetree::ReferenceNode& parent) {
  auto regulator_name = parent.properties().find(kRegulatorName);
  if (regulator_name == parent.properties().end()) {
    FDF_LOG(ERROR, "Regulator node '%s' does not have a name.", parent.name().c_str());
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  auto regulator_node = fuchsia_driver_framework::ParentSpec{{
      .bind_rules =
          {
              fdf::MakeAcceptBindRule(bind_fuchsia_hardware_vreg::SERVICE,
                                      bind_fuchsia_hardware_vreg::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeAcceptBindRule(bind_fuchsia_regulator::NAME,
                                      regulator_name->second.AsString().value()),
          },
      .properties =
          {
              fdf::MakeProperty(bind_fuchsia_hardware_vreg::SERVICE,
                                bind_fuchsia_hardware_vreg::SERVICE_ZIRCONTRANSPORT),
              fdf::MakeProperty(bind_fuchsia_regulator::NAME,
                                regulator_name->second.AsString().value()),
          },
  }};

  child.AddNodeSpec(regulator_node);
  FDF_LOG(DEBUG, "Added regulator node spec of '%s' to '%s'.", parent.name().c_str(),
          child.name().c_str());

  return zx::ok();
}

}  // namespace regulator_visitor_dt

REGISTER_DEVICETREE_VISITOR(regulator_visitor_dt::RegulatorVisitor);
