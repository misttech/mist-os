// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "usb-phy-visitor.h"

#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devicetree/visitors/registration.h>
#include <lib/driver/logging/cpp/logger.h>

#include <regex>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/designware/platform/cpp/bind.h>
#include <bind/fuchsia/hardware/usb/phy/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>

namespace usb_phy_visitor_dt {

UsbPhyVisitor::UsbPhyVisitor() {
  fdf_devicetree::Properties properties = {};
  properties.emplace_back(std::make_unique<fdf_devicetree::ReferenceProperty>(kPhys, kPhyCells));
  properties.emplace_back(std::make_unique<fdf_devicetree::StringListProperty>(kPhyNames));
  parser_ = std::make_unique<fdf_devicetree::PropertyParser>(std::move(properties));
}

bool UsbPhyVisitor::is_match(const std::string& name) {
  // Check that it starts with "usb" and optionally contains a unit address (eg: usb@ffaa0000).
  std::regex name_regex("^usb(@.*)?");
  return std::regex_match(name, name_regex);
}

zx::result<> UsbPhyVisitor::Visit(fdf_devicetree::Node& node,
                                  const devicetree::PropertyDecoder& decoder) {
  if (!is_match(node.name())) {
    return zx::ok();
  }

  zx::result parser_output = parser_->Parse(node);
  if (parser_output.is_error()) {
    FDF_LOG(ERROR, "Usb visitor parse failed for node '%s' : %s", node.name().c_str(),
            parser_output.status_string());
    return parser_output.take_error();
  }

  if (parser_output->find(kPhys) != parser_output->end()) {
    if (!parser_output->contains(kPhyNames)) {
      FDF_LOG(ERROR, "Node '%s' is missing phy-names.", node.name().c_str());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    size_t count = (*parser_output)[kPhys].size();
    if ((*parser_output)[kPhyNames].size() != count) {
      FDF_LOG(ERROR,
              "Node '%s' does not have required number of phy-names. Expected (%zu), actual (%zu).",
              node.name().c_str(), count, (*parser_output)[kPhyNames].size());
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    for (uint32_t index = 0; index < count; index++) {
      auto reference = (*parser_output)[kPhys][index].AsReference();
      if (!reference) {
        FDF_LOG(ERROR, "Node '%s' has invalid phy reference at %d index.", node.name().c_str(),
                index);
        return zx::error(ZX_ERR_INVALID_ARGS);
      }

      auto name = (*parser_output)[kPhyNames][index].AsString();
      if (!name) {
        FDF_LOG(ERROR, "Node '%s' has invalid phy-name property.", node.name().c_str());
        return zx::error(ZX_ERR_INVALID_ARGS);
      }

      auto result = AddChildNodeSpec(node, *name);
      if (result.is_error()) {
        return result.take_error();
      }
    }
  }
  return zx::ok();
}

zx::result<> UsbPhyVisitor::AddChildNodeSpec(fdf_devicetree::Node& child,
                                             std::string_view phy_name) {
  std::vector bind_rules = {
      fdf::MakeAcceptBindRule2(bind_fuchsia_hardware_usb_phy::SERVICE,
                               bind_fuchsia_hardware_usb_phy::SERVICE_DRIVERTRANSPORT),
      fdf::MakeAcceptBindRule2(bind_fuchsia::PLATFORM_DEV_VID,
                               bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC),
      fdf::MakeAcceptBindRule2(bind_fuchsia::PLATFORM_DEV_PID,
                               bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC),

  };
  std::vector bind_properties = {
      fdf::MakeProperty2(bind_fuchsia_hardware_usb_phy::SERVICE,
                         bind_fuchsia_hardware_usb_phy::SERVICE_DRIVERTRANSPORT),
      fdf::MakeProperty2(bind_fuchsia::PLATFORM_DEV_VID,
                         bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC),
      fdf::MakeProperty2(bind_fuchsia::PLATFORM_DEV_PID,
                         bind_fuchsia_platform::BIND_PLATFORM_DEV_PID_GENERIC),

  };

  if (phy_name == "xhci-phy") {
    bind_rules.emplace_back(fdf::MakeAcceptBindRule2(
        bind_fuchsia::PLATFORM_DEV_DID, bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_XHCI));
    bind_properties.emplace_back(fdf::MakeProperty2(
        bind_fuchsia::PLATFORM_DEV_DID, bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_XHCI));
  } else if (phy_name == "dwc2-phy") {
    bind_rules.emplace_back(fdf::MakeAcceptBindRule2(
        bind_fuchsia::PLATFORM_DEV_DID, bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_USB_DWC2));
    bind_properties.emplace_back(fdf::MakeProperty2(
        bind_fuchsia::PLATFORM_DEV_DID, bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_USB_DWC2));
  } else if (phy_name == "dwc3-phy") {
    bind_rules.emplace_back(
        fdf::MakeAcceptBindRule2(bind_fuchsia::PLATFORM_DEV_DID,
                                 bind_fuchsia_designware_platform::BIND_PLATFORM_DEV_DID_DWC3));
    bind_properties.emplace_back(
        fdf::MakeProperty2(bind_fuchsia::PLATFORM_DEV_DID,
                           bind_fuchsia_designware_platform::BIND_PLATFORM_DEV_DID_DWC3));
  } else {
    FDF_LOG(ERROR, "Node '%s' has invalid phy-name '%s'.", child.name().c_str(),
            std::string(phy_name).c_str());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  auto phy_node = fuchsia_driver_framework::ParentSpec2{{bind_rules, bind_properties}};

  child.AddNodeSpec(phy_node);

  FDF_LOG(DEBUG, "Added '%s' bind rules to node '%s'.", std::string(phy_name).c_str(),
          child.name().c_str());
  return zx::ok();
}

}  // namespace usb_phy_visitor_dt

REGISTER_DEVICETREE_VISITOR(usb_phy_visitor_dt::UsbPhyVisitor);
