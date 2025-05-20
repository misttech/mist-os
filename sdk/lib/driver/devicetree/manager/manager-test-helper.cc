// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/driver/devicetree/manager/manager-test-helper.h"

#include <fcntl.h>
#include <lib/syslog/cpp/macros.h>
#include <sys/stat.h>
#include <zircon/assert.h>

#include <memory>
#include <sstream>
#include <utility>

namespace fdf_devicetree::testing {

std::vector<uint8_t> LoadTestBlob(const char* name) {
  int fd = open(name, O_RDONLY);
  ZX_ASSERT_MSG(fd >= 0, "Open failed '%s': %s", name, strerror(errno));

  struct stat stat_out;
  ZX_ASSERT_MSG(fstat(fd, &stat_out) >= 0, "fstat failed: %s", strerror(errno));

  std::vector<uint8_t> vec(stat_out.st_size);
  ssize_t bytes_read = read(fd, vec.data(), stat_out.st_size);
  ZX_ASSERT_MSG(bytes_read >= 0, "Read failed: %s", strerror(errno));

  vec.resize(bytes_read);
  return vec;
}
std::string DebugStringifyProperty(
    const fuchsia_driver_framework::NodePropertyKey& key,
    const std::vector<fuchsia_driver_framework::NodePropertyValue>& values) {
  std::stringstream ret;
  ret << "Key=";

  switch (key.Which()) {
    using Tag = fuchsia_driver_framework::NodePropertyKey::Tag;
    case Tag::kIntValue:
      ret << "Int{" << key.int_value().value() << "}";
      break;
    case Tag::kStringValue:
      ret << "Str{" << key.string_value().value() << "}";
      break;
    default:
      ret << "Unknown{" << static_cast<int>(key.Which()) << "}";
      break;
  }

  for (auto& value : values) {
    ret << " Value=";
    switch (value.Which()) {
      using Tag = fuchsia_driver_framework::NodePropertyValue::Tag;
      case Tag::kBoolValue:
        ret << "Bool{" << value.bool_value().value() << "}";
        break;
      case Tag::kEnumValue:
        ret << "Enum{" << value.enum_value().value() << "}";
        break;
      case Tag::kIntValue:
        ret << "Int{" << value.int_value().value() << "}";
        break;
      case Tag::kStringValue:
        ret << "String{" << value.string_value().value() << "}";
        break;
      default:
        ret << "Unknown{" << static_cast<int>(value.Which()) << "}";
        break;
    }
  }

  return ret.str();
}

std::string DebugStringifyProperty(
    const std::string& key,
    const std::vector<fuchsia_driver_framework::NodePropertyValue>& values) {
  std::stringstream ret;
  ret << "Key=" << key;

  for (auto& value : values) {
    ret << " Value=";
    switch (value.Which()) {
      using Tag = fuchsia_driver_framework::NodePropertyValue::Tag;
      case Tag::kBoolValue:
        ret << "Bool{" << value.bool_value().value() << "}";
        break;
      case Tag::kEnumValue:
        ret << "Enum{" << value.enum_value().value() << "}";
        break;
      case Tag::kIntValue:
        ret << "Int{" << value.int_value().value() << "}";
        break;
      case Tag::kStringValue:
        ret << "String{" << value.string_value().value() << "}";
        break;
      default:
        ret << "Unknown{" << static_cast<int>(value.Which()) << "}";
        break;
    }
  }

  return ret.str();
}

bool CheckHasProperties(
    std::vector<fuchsia_driver_framework::NodeProperty> expected,
    const std::vector<::fuchsia_driver_framework::NodeProperty>& node_properties,
    bool allow_additional_properties) {
  bool result = true;
  for (auto& property : node_properties) {
    auto iter = std::find(expected.begin(), expected.end(), property);
    if (iter == expected.end()) {
      if (!allow_additional_properties) {
        FX_LOGS(ERROR) << "Unexpected property: "
                       << DebugStringifyProperty(property.key(), {property.value()});
        result = false;
      }
    } else {
      expected.erase(iter);
    }
  }

  if (!expected.empty()) {
    FX_LOGS(ERROR) << "All expected properties are not present.";
    for (auto& property : expected) {
      FX_LOGS(ERROR) << "Property expected: "
                     << DebugStringifyProperty(property.key(), {property.value()});
    }
    result = false;
  }

  return result;
}

bool CheckHasProperties(
    std::vector<fuchsia_driver_framework::NodeProperty2> expected,
    const std::vector<::fuchsia_driver_framework::NodeProperty2>& node_properties,
    bool allow_additional_properties) {
  bool result = true;
  for (auto& property : node_properties) {
    auto iter = std::find(expected.begin(), expected.end(), property);
    if (iter == expected.end()) {
      if (!allow_additional_properties) {
        FX_LOGS(ERROR) << "Unexpected property: "
                       << DebugStringifyProperty(property.key(), {property.value()});
        result = false;
      }
    } else {
      expected.erase(iter);
    }
  }

  if (!expected.empty()) {
    FX_LOGS(ERROR) << "All expected properties are not present.";
    for (auto& property : expected) {
      FX_LOGS(ERROR) << "Property expected: "
                     << DebugStringifyProperty(property.key(), {property.value()});
    }
    result = false;
  }

  return result;
}

bool CheckHasBindRules(std::vector<fuchsia_driver_framework::BindRule> expected,
                       const std::vector<::fuchsia_driver_framework::BindRule>& node_rules,
                       bool allow_additional_rules) {
  bool result = true;
  for (auto& rule : node_rules) {
    auto iter = std::find(expected.begin(), expected.end(), rule);
    if (iter == expected.end()) {
      if (!allow_additional_rules) {
        FX_LOGS(ERROR) << "Unexpected bind rule: "
                       << DebugStringifyProperty(rule.key(), rule.values());
        result = false;
      }
    } else {
      expected.erase(iter);
    }
  }

  if (!expected.empty()) {
    FX_LOGS(ERROR) << "All expected bind rules are not present.";
    for (auto& rule : expected) {
      FX_LOGS(ERROR) << "Rule expected: " << DebugStringifyProperty(rule.key(), rule.values());
    }
    result = false;
  }

  return result;
}

bool CheckHasBindRules(std::vector<fuchsia_driver_framework::BindRule2> expected,
                       const std::vector<::fuchsia_driver_framework::BindRule2>& node_rules,
                       bool allow_additional_rules) {
  bool result = true;
  for (auto& rule : node_rules) {
    auto iter = std::find(expected.begin(), expected.end(), rule);
    if (iter == expected.end()) {
      if (!allow_additional_rules) {
        FX_LOGS(ERROR) << "Unexpected bind rule: "
                       << DebugStringifyProperty(rule.key(), rule.values());
        result = false;
      }
    } else {
      expected.erase(iter);
    }
  }

  if (!expected.empty()) {
    FX_LOGS(ERROR) << "All expected bind rules are not present.";
    for (auto& rule : expected) {
      FX_LOGS(ERROR) << "Rule expected: " << DebugStringifyProperty(rule.key(), rule.values());
    }
    result = false;
  }

  return result;
}

void FakeEnvWrapper::Bind(
    fdf::ServerEnd<fuchsia_hardware_platform_bus::PlatformBus> pbus_server_end,
    fidl::ServerEnd<fuchsia_driver_framework::CompositeNodeManager> mgr_server_end,
    fidl::ServerEnd<fuchsia_driver_framework::Node> node_server_end) {
  fdf::BindServer(fdf::Dispatcher::GetCurrent()->get(), std::move(pbus_server_end), &pbus_);
  fidl::BindServer(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(mgr_server_end),
                   &mgr_);
  fidl::BindServer(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(node_server_end),
                   &node_);
}

size_t FakeEnvWrapper::pbus_node_size() { return pbus_.nodes().size(); }

size_t FakeEnvWrapper::non_pbus_node_size() { return node_.requests().size(); }

size_t FakeEnvWrapper::mgr_requests_size() { return mgr_.requests().size(); }

FakeCompositeNodeManager::AddSpecRequest FakeEnvWrapper::mgr_requests_at(size_t index) {
  return mgr_.requests()[index];
}

fuchsia_hardware_platform_bus::Node FakeEnvWrapper::pbus_nodes_at(size_t index) {
  return pbus_.nodes()[index];
}

std::shared_ptr<fidl::Request<fuchsia_driver_framework::Node::AddChild>>
FakeEnvWrapper::non_pbus_nodes_at(size_t index) {
  return node_.requests()[index];
}

zx::result<> ManagerTestHelper::DoPublish(Manager& manager) {
  auto pbus_endpoints = fdf::Endpoints<fuchsia_hardware_platform_bus::PlatformBus>::Create();
  auto mgr_endpoints = fidl::Endpoints<fuchsia_driver_framework::CompositeNodeManager>::Create();
  auto node_endpoints = fidl::Endpoints<fuchsia_driver_framework::Node>::Create();
  node_.Bind(std::move(node_endpoints.client));

  env_.SyncCall(&FakeEnvWrapper::Bind, std::move(pbus_endpoints.server),
                std::move(mgr_endpoints.server), std::move(node_endpoints.server));
  pbus_.Bind(std::move(pbus_endpoints.client));

  return manager.PublishDevices(pbus_, std::move(mgr_endpoints.client), node_);
}

}  // namespace fdf_devicetree::testing
