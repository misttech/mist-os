// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "spmi-visitor.h"

#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/common-types.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <stdlib.h>

#include <utility>

#include <bind/fuchsia/hardware/spmi/cpp/bind.h>
#include <bind/fuchsia/spmi/cpp/bind.h>

#include "spmi.h"

namespace {

template <typename T>
zx::result<std::pair<uint32_t, uint32_t>> GetAddressAndSizeCells(const T& node) {
  auto address_cells = node.properties().find("#address-cells");
  if (address_cells == node.properties().end() || !address_cells->second.AsUint32()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  auto size_cells = node.properties().find("#size-cells");
  if (size_cells == node.properties().end() || !size_cells->second.AsUint32()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  return zx::ok(std::pair<uint32_t, uint32_t>(*address_cells->second.AsUint32(),
                                              *size_cells->second.AsUint32()));
}

std::vector<std::string_view> GetRegNames(const fdf_devicetree::ChildNode& node) {
  std::vector<std::string_view> reg_names;

  auto reg_names_property = node.properties().find("reg-names");
  if (reg_names_property != node.properties().end()) {
    std::optional reg_names_list = reg_names_property->second.AsStringList();
    if (reg_names_list) {
      reg_names = {reg_names_list->begin(), reg_names_list->end()};
    }
  }

  return reg_names;
}

// Returns the hexadecimal number following '@' in a devicetree node name.
zx::result<uint64_t> GetAddressSuffix(const std::string& name) {
  const size_t at_position = name.rfind('@');
  if (at_position == std::string::npos) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  char* endptr = nullptr;
  const uint64_t value = strtoul(name.data() + at_position + 1, &endptr, 16);
  if (*endptr == '\0') {
    return zx::ok(value);
  }
  return zx::error(ZX_ERR_INVALID_ARGS);
}

}  // namespace

namespace spmi_dt {

zx::result<> SpmiVisitor::FinalizeNode(fdf_devicetree::Node& node) {
  if (!node.name().starts_with("spmi@")) {
    return zx::ok();
  }

  const auto cells = GetAddressAndSizeCells(node);
  if (cells.is_error() || *cells != std::pair<uint32_t, uint32_t>{2, 0}) {
    FDF_LOG(ERROR, "Invalid #address-cells or #size-cells for SPMI controller \"%s\"",
            node.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  fuchsia_hardware_spmi::ControllerInfo controller{{.id = ++controller_id_}};

  uint16_t used_target_ids = 0;
  for (const fdf_devicetree::ChildNode& child : node.children()) {
    zx::result<uint64_t> address_suffix = GetAddressSuffix(child.name());
    if (address_suffix.is_error()) {
      FDF_LOG(ERROR, "SPMI target \"%s\" address suffix is invalid", child.name().c_str());
      return address_suffix.take_error();
    }

    auto reg_property = child.properties().find("reg");
    if (reg_property == child.properties().end()) {
      FDF_LOG(ERROR, "SPMI target \"%s\" has no reg property", child.name().c_str());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    fdf_devicetree::Uint32Array reg_array(reg_property->second.AsBytes());
    if (reg_array.size() != 2) {
      FDF_LOG(ERROR, "SPMI target \"%s\" reg property has invalid size: %zu", child.name().c_str(),
              reg_array.size());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    const uint32_t target_id = reg_array[0];
    const uint32_t target_type = reg_array[1];

    if (target_id >= fuchsia_hardware_spmi::kMaxTargets) {
      FDF_LOG(ERROR, "SPMI target ID %u for \"%s\" out of range", target_id, node.name().c_str());
      return zx::error(ZX_ERR_ALREADY_EXISTS);
    }
    if (target_type != SPMI_USID) {
      FDF_LOG(ERROR, "Unsupported SPMI target type %u for \"%s\"", target_id, node.name().c_str());
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    }
    if (target_id != address_suffix) {
      FDF_LOG(ERROR, "SPMI target \"%s\" has mismatched reg and address suffix",
              node.name().c_str());
      return zx::error(ZX_ERR_ALREADY_EXISTS);
    }

    if (used_target_ids & (1 << target_id)) {
      FDF_LOG(ERROR, "Duplicate SPMI target ID %u for \"%s\"", target_id, node.name().c_str());
      return zx::error(ZX_ERR_ALREADY_EXISTS);
    }

    used_target_ids |= (1 << target_id);

    zx::result<fuchsia_hardware_spmi::TargetInfo> target = ParseTarget(target_id, child);
    if (target.is_error()) {
      return target.take_error();
    }

    if (!controller.targets()) {
      controller.targets().emplace();
    }
    controller.targets()->push_back(*std::move(target));
  }

  fit::result<fidl::Error, std::vector<uint8_t>> metadata = fidl::Persist(controller);
  if (metadata.is_error()) {
    FDF_LOG(ERROR, "Failed to persist SPMI controller metadata: %s",
            metadata.error_value().FormatDescription().c_str());
    return zx::error(metadata.error_value().status());
  }

  fuchsia_hardware_platform_bus::Metadata pbus_metadata{{
      .type = fuchsia_hardware_spmi::kControllerInfoMetadataType,
      .data = *std::move(metadata),
  }};
  node.AddMetadata(std::move(pbus_metadata));

  return zx::ok();
}

zx::result<fuchsia_hardware_spmi::TargetInfo> SpmiVisitor::ParseTarget(
    uint32_t target_id, const fdf_devicetree::ChildNode& node) const {
  std::vector<std::string_view> reg_names = GetRegNames(node);
  if (reg_names.size() > 1) {
    FDF_LOG(ERROR, "SPMI target \"%s\" has mismatched reg and reg-names properties",
            node.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  fuchsia_hardware_spmi::TargetInfo target{{.id = target_id}};
  if (!reg_names.empty()) {
    target.name() = reg_names[0];
  }

  if (node.GetNode()->children().empty()) {
    // This target has no sub-target children, so add a composite node spec for it.
    fuchsia_driver_framework::ParentSpec target_spec{{
        .bind_rules =
            {
                fdf::MakeAcceptBindRule(bind_fuchsia_hardware_spmi::TARGETSERVICE,
                                        bind_fuchsia_hardware_spmi::TARGETSERVICE_ZIRCONTRANSPORT),
                fdf::MakeAcceptBindRule(bind_fuchsia_spmi::CONTROLLER_ID, controller_id_),
                fdf::MakeAcceptBindRule(bind_fuchsia_spmi::TARGET_ID, target_id),
            },
        .properties =
            {
                fdf::MakeProperty(bind_fuchsia_hardware_spmi::TARGETSERVICE,
                                  bind_fuchsia_hardware_spmi::TARGETSERVICE_ZIRCONTRANSPORT),
                fdf::MakeProperty(bind_fuchsia_spmi::TARGET_ID, target_id),
            },
    }};

    if (!reg_names.empty()) {
      target_spec.properties().push_back(
          fdf::MakeProperty(bind_fuchsia_spmi::TARGET_NAME, reg_names[0]));
    }

    node.GetNode()->AddNodeSpec(target_spec);
    return zx::ok(target);
  }

  const auto cells = GetAddressAndSizeCells(node);
  if (cells.is_error() || *cells != std::pair<uint32_t, uint32_t>{1, 1}) {
    FDF_LOG(ERROR, "Invalid #address-cells or #size-cells for SPMI target \"%s\"",
            node.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  std::vector<fuchsia_hardware_spmi::SubTargetInfo> sub_targets;
  for (const fdf_devicetree::ChildNode& child : node.GetNode()->children()) {
    zx::result<std::vector<fuchsia_hardware_spmi::SubTargetInfo>> sub_target_regions =
        ParseSubTarget(target, child);
    if (sub_target_regions.is_error()) {
      return sub_target_regions.take_error();
    }
    sub_targets.insert(sub_targets.end(), sub_target_regions->begin(), sub_target_regions->end());
  }

  target.sub_targets().emplace(std::move(sub_targets));
  return zx::ok(target);
}

zx::result<std::vector<fuchsia_hardware_spmi::SubTargetInfo>> SpmiVisitor::ParseSubTarget(
    const fuchsia_hardware_spmi::TargetInfo& parent, const fdf_devicetree::ChildNode& node) const {
  ZX_DEBUG_ASSERT(parent.id());

  zx::result<uint64_t> address_suffix = GetAddressSuffix(node.name());
  if (address_suffix.is_error()) {
    FDF_LOG(ERROR, "SPMI sub-target \"%s\" address suffix is invalid", node.name().c_str());
    return address_suffix.take_error();
  }

  auto reg_property = node.properties().find("reg");
  if (reg_property == node.properties().end()) {
    FDF_LOG(ERROR, "SPMI sub-target \"%s\" has no reg property", node.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  fdf_devicetree::Uint32Array reg_array(reg_property->second.AsBytes());
  if (reg_array.size() == 0 || reg_array.size() % 2 != 0) {
    FDF_LOG(ERROR, "SPMI sub-target \"%s\" has invalid reg size %zu", node.name().c_str(),
            reg_array.size());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if (reg_array[0] != *address_suffix) {
    FDF_LOG(ERROR, "SPMI sub-target \"%s\" has mismatched reg and address suffix",
            node.name().c_str());
    return address_suffix.take_error();
  }

  std::vector<std::string_view> reg_names = GetRegNames(node);
  if (!reg_names.empty() && reg_names.size() != reg_array.size() / 2) {
    FDF_LOG(ERROR, "SPMI sub-target \"%s\" has mismatched reg and reg-names properties",
            node.name().c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  std::vector<fuchsia_hardware_spmi::SubTargetInfo> sub_targets;
  for (size_t i = 0; i < reg_array.size(); i += 2) {
    const uint32_t address = reg_array[i];
    const uint32_t size = reg_array[i + 1];

    uint32_t address_plus_size{};
    const bool overflow = add_overflow(address, size, &address_plus_size);
    if (size == 0 || overflow || address_plus_size > UINT16_MAX + 1) {
      FDF_LOG(ERROR, "SPMI sub-target \"%s\" has invalid address (0x%04x) or size (%u)",
              node.name().c_str(), address, size);
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    fuchsia_hardware_spmi::SubTargetInfo sub_target{{
        .address = static_cast<uint16_t>(address),
        .size = size,
    }};

    fuchsia_driver_framework::ParentSpec sub_target_spec{{
        .bind_rules =
            {
                fdf::MakeAcceptBindRule(
                    bind_fuchsia_hardware_spmi::SUBTARGETSERVICE,
                    bind_fuchsia_hardware_spmi::SUBTARGETSERVICE_ZIRCONTRANSPORT),
                fdf::MakeAcceptBindRule(bind_fuchsia_spmi::CONTROLLER_ID, controller_id_),
                fdf::MakeAcceptBindRule(bind_fuchsia_spmi::TARGET_ID,
                                        static_cast<uint32_t>(*parent.id())),
                fdf::MakeAcceptBindRule(bind_fuchsia_spmi::SUB_TARGET_ADDRESS, address),
            },
        .properties =
            {
                fdf::MakeProperty(bind_fuchsia_hardware_spmi::SUBTARGETSERVICE,
                                  bind_fuchsia_hardware_spmi::SUBTARGETSERVICE_ZIRCONTRANSPORT),
                fdf::MakeProperty(bind_fuchsia_spmi::TARGET_ID,
                                  static_cast<uint32_t>(*parent.id())),
                fdf::MakeProperty(bind_fuchsia_spmi::SUB_TARGET_ADDRESS, address),
            },
    }};

    if (parent.name()) {
      sub_target_spec.properties().push_back(
          fdf::MakeProperty(bind_fuchsia_spmi::TARGET_NAME, *parent.name()));
    }

    if (!reg_names.empty()) {
      const std::string_view sub_target_name = reg_names[i / 2];
      sub_target.name() = sub_target_name;
      sub_target_spec.properties().push_back(
          fdf::MakeProperty(bind_fuchsia_spmi::SUB_TARGET_NAME, sub_target_name));
    }

    sub_targets.push_back(std::move(sub_target));

    node.GetNode()->AddNodeSpec(sub_target_spec);
  }

  return zx::ok(sub_targets);
}

}  // namespace spmi_dt

REGISTER_DEVICETREE_VISITOR(spmi_dt::SpmiVisitor);
