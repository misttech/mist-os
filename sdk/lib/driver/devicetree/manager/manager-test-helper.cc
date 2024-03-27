// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "manager-test-helper.h"

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

void FakeEnvWrapper::Bind(
    fdf::ServerEnd<fuchsia_hardware_platform_bus::PlatformBus> pbus_endpoints_server,
    fidl::ServerEnd<fuchsia_driver_framework::CompositeNodeManager> mgr_endpoints_server,
    fidl::ServerEnd<fuchsia_driver_framework::Node> node_endpoint_server) {
  fdf::BindServer(fdf::Dispatcher::GetCurrent()->get(), std::move(pbus_endpoints_server), &pbus_);
  fidl::BindServer(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                   std::move(mgr_endpoints_server), &mgr_);
  fidl::BindServer(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                   std::move(node_endpoint_server), &node_);
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

void ManagerTestHelper::ConnectLogger(std::string_view tag) {
  zx::socket client_end, server_end;
  zx_status_t status = zx::socket::create(ZX_SOCKET_DATAGRAM, &client_end, &server_end);
  ZX_ASSERT(status == ZX_OK);

  auto connect_result = component::Connect<fuchsia_logger::LogSink>();
  ZX_ASSERT(connect_result.is_ok());

  fidl::WireSyncClient<fuchsia_logger::LogSink> log_sink;
  log_sink.Bind(std::move(*connect_result));
  auto sink_result = log_sink->ConnectStructured(std::move(server_end));
  ZX_ASSERT(sink_result.ok());

  logger_ = std::make_unique<fdf::Logger>(tag, 0, std::move(client_end),
                                          fidl::WireClient<fuchsia_logger::LogSink>());
  fdf::Logger::SetGlobalInstance(logger_.get());
}

zx::result<> ManagerTestHelper::DoPublish(Manager& manager) {
  auto pbus_endpoints = fdf::CreateEndpoints<fuchsia_hardware_platform_bus::PlatformBus>();
  ZX_ASSERT(pbus_endpoints.is_ok());
  auto mgr_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::CompositeNodeManager>();
  ZX_ASSERT(mgr_endpoints.is_ok());
  auto node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  ZX_ASSERT(node_endpoints.is_ok());
  node_.Bind(std::move(node_endpoints->client));

  env_.SyncCall(&FakeEnvWrapper::Bind, std::move(pbus_endpoints->server),
                std::move(mgr_endpoints->server), std::move(node_endpoints->server));
  pbus_.Bind(std::move(pbus_endpoints->client));

  return manager.PublishDevices(pbus_, std::move(mgr_endpoints->client), node_);
}

}  // namespace fdf_devicetree::testing
