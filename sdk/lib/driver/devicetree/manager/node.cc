// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/driver/devicetree/manager/node.h"

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <lib/driver/component/cpp/composite_node_spec.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/logging/cpp/logger.h>
#include <zircon/errors.h>

#include <optional>
#include <string>
#include <vector>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>

namespace fdf {
using namespace fuchsia_driver_framework;
}

namespace fdf_devicetree {

constexpr const char kPhandleProp[] = "phandle";

NodeManager::~NodeManager() = default;

Node::Node(Node *parent, const std::string_view name, devicetree::Properties properties,
           uint32_t id, NodeManager *manager)
    : parent_(parent), name_(name), id_(id), manager_(manager) {
  ZX_ASSERT(manager_);

  if (parent_) {
    parent_->children_.push_back(this);
  } else {
    name_ = "dt-root";
  }

  fdf_name_ = name_;
  // '@' and ',' are not a valid character in Node names as per driver framework.
  std::replace(fdf_name_.begin(), fdf_name_.end(), '@', '-');
  std::replace(fdf_name_.begin(), fdf_name_.end(), ',', '-');

  pbus_node_.did() = bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_DEVICETREE;
  pbus_node_.vid() = bind_fuchsia_platform::BIND_PLATFORM_DEV_VID_GENERIC;
  pbus_node_.instance_id() = id;
  pbus_node_.name() = fdf_name_;

  for (auto property : properties) {
    properties_.emplace(property.name, property.value);
  }

  // Get phandle if exists.
  const auto phandle_prop = properties_.find(kPhandleProp);
  if (phandle_prop != properties_.end()) {
    if (phandle_prop->second.AsUint32() != std::nullopt) {
      phandle_ = phandle_prop->second.AsUint32();
    } else {
      FDF_LOG(WARNING, "Node '%s' has invalid phandle property", name_.c_str());
    }
  }
}

void Node::AddBindProperty(fuchsia_driver_framework::NodeProperty2 prop) {
  node_properties_.emplace_back(std::move(prop));
}

void Node::AddMmio(fuchsia_hardware_platform_bus::Mmio mmio) {
  if (!pbus_node_.mmio()) {
    pbus_node_.mmio() = std::vector<fuchsia_hardware_platform_bus::Mmio>();
  }
  pbus_node_.mmio()->emplace_back(std::move(mmio));
  add_platform_device_ = true;
}

void Node::AddBti(fuchsia_hardware_platform_bus::Bti bti) {
  if (!pbus_node_.bti()) {
    pbus_node_.bti() = std::vector<fuchsia_hardware_platform_bus::Bti>();
  }
  pbus_node_.bti()->emplace_back(std::move(bti));
  add_platform_device_ = true;
}

void Node::AddIrq(fuchsia_hardware_platform_bus::Irq irq) {
  if (!pbus_node_.irq()) {
    pbus_node_.irq() = std::vector<fuchsia_hardware_platform_bus::Irq>();
  }
  pbus_node_.irq()->emplace_back(std::move(irq));
  add_platform_device_ = true;
}

void Node::AddMetadata(fuchsia_hardware_platform_bus::Metadata metadata) {
  if (!pbus_node_.metadata()) {
    pbus_node_.metadata() = std::vector<fuchsia_hardware_platform_bus::Metadata>();
  }
  pbus_node_.metadata()->emplace_back(std::move(metadata));
  add_platform_device_ = true;
}

void Node::AddBootMetadata(fuchsia_hardware_platform_bus::BootMetadata boot_metadata) {
  if (!pbus_node_.boot_metadata()) {
    pbus_node_.boot_metadata() = std::vector<fuchsia_hardware_platform_bus::BootMetadata>();
  }
  pbus_node_.boot_metadata()->emplace_back(std::move(boot_metadata));
  add_platform_device_ = true;
}

void Node::AddNodeSpec(const fuchsia_driver_framework::ParentSpec2 &spec) {
  parents_.emplace_back(spec);
  composite_ = true;
}

void Node::AddSmc(fuchsia_hardware_platform_bus::Smc smc) {
  if (!pbus_node_.smc()) {
    pbus_node_.smc() = std::vector<fuchsia_hardware_platform_bus::Smc>();
  }
  pbus_node_.smc()->emplace_back(std::move(smc));
  add_platform_device_ = true;
}

void Node::AddPowerConfig(fuchsia_hardware_power::PowerElementConfiguration power_config) {
  if (!pbus_node_.power_config()) {
    pbus_node_.power_config() = std::vector<fuchsia_hardware_power::PowerElementConfiguration>();
  }
  pbus_node_.power_config()->emplace_back(std::move(power_config));
  add_platform_device_ = true;
}

uint32_t Node::GetPublishIndex() const { return manager_->GetPublishIndex(id()); }

zx::result<> Node::ChangePublishOrder(uint32_t new_index) {
  return manager_->ChangePublishOrder(id(), new_index);
}

zx::result<> Node::Publish(fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus> &pbus,
                           fidl::SyncClient<fuchsia_driver_framework::CompositeNodeManager> &mgr,
                           fidl::SyncClient<fuchsia_driver_framework::Node> &fdf_node) {
  if (node_properties_.empty() && !composite_ && !add_platform_device_) {
    FDF_LOG(
        DEBUG,
        "Not publishing node '%.*s' because it has no node properties, and no platform resources, and it is not a composite node.",
        static_cast<int>(name().size()), name().data());
    return zx::ok();
  }

  auto status_property = properties_.find("status");
  if (status_property != properties_.end()) {
    auto status_string = status_property->second.AsString();
    if (status_string && (*status_string != "okay")) {
      FDF_LOG(DEBUG, "Not publishing node '%.*s' because its status is %.*s.",
              static_cast<int>(name().size()), name().data(),
              static_cast<int>(status_string->size()), status_string->data());
      return zx::ok();
    }
  }

  // Nodes are published as per below logic -
  // 1. Node has platform resources
  //     a. Node references other nodes -> PlatformBus.NodeAdd + CompositeNodeManager.AddSpec
  //     b. Node does not reference other nodes -> PlatformBus.NodeAdd
  // 2. Node does not have platform resources
  //     a. Node has bind properties (i.e. compatible string)
  //        i. Node references other nodes ->  Node.AddChild + CompositeNodeManager.AddSpec
  //        ii. Node does not reference other nodes -> Node.AddChild
  //     b. Node has no bind properties
  //        i. Node references other nodes -> CompositeNodeManager.AddSpec
  //        ii. Node does not reference other nodes -> Not published
  //

  bool add_non_platform_device = !add_platform_device_ && !node_properties_.empty();

  if (add_platform_device_) {
    FDF_LOG(DEBUG, "Adding node '%s' to pbus with instance id %d.", fdf_name().c_str(), id_);

    // Pass properties to pbus node directly if we are not adding a composite spec.
    if (!composite_) {
      pbus_node_.properties() = node_properties_;
    }

    fdf::Arena arena('PBUS');
    fidl::Arena fidl_arena;
    auto result = pbus.buffer(arena)->NodeAdd(fidl::ToWire(fidl_arena, pbus_node_));
    if (!result.ok()) {
      FDF_LOG(ERROR, "NodeAdd request failed: %s", result.FormatDescription().data());
      return zx::error(result.status());
    }
    if (result->is_error()) {
      FDF_LOG(ERROR, "NodeAdd failed: %s", zx_status_get_string(result->error_value()));
      return zx::error(result->error_value());
    }
  } else if (add_non_platform_device) {
    FDF_LOG(DEBUG, "Adding node '%s' as board driver child.", fdf_name().c_str());
    fuchsia_driver_framework::NodeAddArgs node_add_args;
    node_add_args.name() = fdf_name();

    node_add_args.properties2() = node_properties_;

    auto [client_end, server_end] =
        fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();
    auto result = fdf_node->AddChild({std::move(node_add_args), std::move(server_end), {}});
    if (result.is_error()) {
      FDF_LOG(ERROR, "AddChild request failed: %s",
              result.error_value().FormatDescription().c_str());
      return zx::error(ZX_ERR_INTERNAL);
    }
    node_controller_.Bind(std::move(client_end));
  }

  // Add composite node spec if composite.
  if (composite_) {
    if (add_platform_device_) {
      // Construct the platform bus node.
      fdf::ParentSpec2 platform_node;
      platform_node.properties() = node_properties_;
      auto additional_node_properties = std::vector<fdf::NodeProperty2>{
          fdf::MakeProperty2(bind_fuchsia::PROTOCOL, bind_fuchsia_platform::BIND_PROTOCOL_DEVICE),
          fdf::MakeProperty2(bind_fuchsia::PLATFORM_DEV_VID,
                             bind_fuchsia_platform::BIND_PLATFORM_DEV_VID_GENERIC),
          fdf::MakeProperty2(bind_fuchsia::PLATFORM_DEV_DID,
                             bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_DEVICETREE),
          fdf::MakeProperty2(bind_fuchsia::PLATFORM_DEV_INSTANCE_ID, id_),
      };
      platform_node.properties().insert(platform_node.properties().end(),
                                        additional_node_properties.begin(),
                                        additional_node_properties.end());

      platform_node.bind_rules() = std::vector<fdf::BindRule2>{
          fdf::MakeAcceptBindRule2(bind_fuchsia::PROTOCOL,
                                   bind_fuchsia_platform::BIND_PROTOCOL_DEVICE),
          fdf::MakeAcceptBindRule2(bind_fuchsia::PLATFORM_DEV_VID,
                                   bind_fuchsia_platform::BIND_PLATFORM_DEV_VID_GENERIC),
          fdf::MakeAcceptBindRule2(bind_fuchsia::PLATFORM_DEV_DID,
                                   bind_fuchsia_platform::BIND_PLATFORM_DEV_DID_DEVICETREE),
          fdf::MakeAcceptBindRule2(bind_fuchsia::PLATFORM_DEV_INSTANCE_ID, id_),
      };
      parents_.insert(parents_.begin(), std::move(platform_node));
    } else if (add_non_platform_device) {
      // Construct the non platform bus node.
      fdf::ParentSpec2 non_pbus_node;
      non_pbus_node.properties() = node_properties_;

      for (auto &node_property : node_properties_) {
        fdf::BindRule2 bind_rule = {node_property.key(),
                                    fuchsia_driver_framework::Condition::kAccept,
                                    {node_property.value()}};
        non_pbus_node.bind_rules().emplace_back(std::move(bind_rule));
      }
      parents_.insert(parents_.begin(), std::move(non_pbus_node));
    }

    FDF_LOG(DEBUG, "Adding composite node spec to '%s' with %zu parents.", fdf_name().data(),
            parents_.size());

    fdf::CompositeNodeSpec group{{
        .name = fdf_name() + "_group",
        .parents2 = std::move(parents_),
    }};

    auto devicegroup_result = mgr->AddSpec({std::move(group)});
    if (devicegroup_result.is_error()) {
      FDF_LOG(ERROR, "Failed to create composite node: %s",
              devicegroup_result.error_value().FormatDescription().data());
      return zx::error(devicegroup_result.error_value().is_framework_error()
                           ? devicegroup_result.error_value().framework_error().status()
                           : ZX_ERR_INVALID_ARGS);
    }
  }

  return zx::ok();
}

zx::result<ReferenceNode> Node::GetReferenceNode(Phandle parent) {
  return manager_->GetReferenceNode(parent);
}

ParentNode Node::parent() const { return ParentNode(parent_); }

std::vector<ChildNode> Node::children() {
  std::vector<ChildNode> children;
  children.reserve(children_.size());
  for (Node *child : children_) {
    children.emplace_back(child);
  }
  return children;
}

ParentNode ReferenceNode::parent() const { return node_->parent(); }

}  // namespace fdf_devicetree
