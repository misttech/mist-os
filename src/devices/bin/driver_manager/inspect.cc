// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/inspect.h"

#include <lib/ddk/driver.h>
#include <lib/inspect/component/cpp/component.h>
#include <lib/inspect/component/cpp/service.h>

#include <utility>

#include "src/storage/lib/vfs/cpp/service.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"
#include "src/storage/lib/vfs/cpp/vnode.h"

namespace driver_manager {

InspectManager::InspectManager(async_dispatcher_t* dispatcher) : inspector_(dispatcher, {}) {
  inspector_.inspector().CreateStatsNode();
  info_ = fbl::MakeRefCounted<Info>(root_node());
}

DeviceInspect InspectManager::CreateDevice(std::string name, uint32_t protocol_id) {
  return DeviceInspect(info_, std::move(name), protocol_id);
}

DeviceInspect::DeviceInspect(fbl::RefPtr<InspectManager::Info> info, std::string name,
                             uint32_t protocol_id)
    : info_(std::move(info)), protocol_id_(protocol_id), name_(std::move(name)) {
  device_node_ = info_->devices.CreateChild(name_);
  // Increment device count.
  info_->device_count.Add(1);
}

DeviceInspect::~DeviceInspect() {
  if (info_) {
    // Decrement device count.
    info_->device_count.Subtract(1);
  }
}

DeviceInspect DeviceInspect::CreateChild(std::string name, uint32_t protocol_id) {
  return DeviceInspect(info_, std::move(name), protocol_id);
}

void DeviceInspect::SetStaticValues(
    const std::string& topological_path, uint32_t protocol_id, const std::string& type,
    const cpp20::span<const fuchsia_driver_framework::wire::NodeProperty2>& properties,
    const std::string& driver_url) {
  protocol_id_ = protocol_id;
  device_node_.CreateString("topological_path", topological_path, &static_values_);
  device_node_.CreateUint("protocol_id", protocol_id, &static_values_);
  device_node_.CreateString("type", type, &static_values_);
  device_node_.CreateString("driver", driver_url, &static_values_);

  inspect::Node properties_array;

  // Add a node only if there are any `props`
  if (!properties.empty()) {
    properties_array = device_node_.CreateChild("properties");
  }

  for (uint32_t i = 0; i < properties.size(); ++i) {
    auto inspect_property = properties_array.CreateChild(std::to_string(i));
    auto& property = properties[i];
    inspect_property.CreateString("key", std::string(property.key.get()), &static_values_);

    switch (property.value.Which()) {
      case fuchsia_driver_framework::wire::NodePropertyValue::Tag::kStringValue:
        inspect_property.CreateString("value", std::string(property.value.string_value().get()),
                                      &static_values_);
        break;
      case fuchsia_driver_framework::wire::NodePropertyValue::Tag::kIntValue:
        inspect_property.CreateUint("value", property.value.int_value(), &static_values_);
        break;
      case fuchsia_driver_framework::wire::NodePropertyValue::Tag::kEnumValue:
        inspect_property.CreateString("value", std::string(property.value.enum_value().get()),
                                      &static_values_);
        break;
      case fuchsia_driver_framework::wire::NodePropertyValue::Tag::kBoolValue:
        inspect_property.CreateBool("value", property.value.bool_value(), &static_values_);
        break;
      default: {
        inspect_property.CreateString("value", "UNKNOWN VALUE TYPE", &static_values_);
        break;
      }
    }
    static_values_.emplace(std::move(inspect_property));
  }

  // Place the node into value list as props will not change in the lifetime of the device.
  if (!properties.empty()) {
    static_values_.emplace(std::move(properties_array));
  }
}

}  // namespace driver_manager
