// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_MANAGER_MANAGER_TEST_HELPER_H_
#define LIB_DRIVER_DEVICETREE_MANAGER_MANAGER_TEST_HELPER_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/scoped_global_logger.h>

#include <memory>

#include "lib/driver/devicetree/manager/manager.h"

namespace fdf_devicetree::testing {

// Load the file |name| into a vector and return it.
std::vector<uint8_t> LoadTestBlob(const char* name);

bool CheckHasProperties(
    std::vector<fuchsia_driver_framework::NodeProperty> expected,
    const std::vector<::fuchsia_driver_framework::NodeProperty>& node_properties,
    bool allow_additional_properties);

bool CheckHasBindRules(std::vector<fuchsia_driver_framework::BindRule> expected,
                       const std::vector<::fuchsia_driver_framework::BindRule>& node_rules,
                       bool allow_additional_rules);

std::string DebugStringifyProperty(
    const fuchsia_driver_framework::NodePropertyKey& key,
    const std::vector<fuchsia_driver_framework::NodePropertyValue>& values);

class FakePlatformBus final : public fdf::Server<fuchsia_hardware_platform_bus::PlatformBus> {
 public:
  void NodeAdd(NodeAddRequest& request, NodeAddCompleter::Sync& completer) override {
    nodes_.emplace_back(std::move(request.node()));
    completer.Reply(zx::ok());
  }

  void AddCompositeNodeSpec(AddCompositeNodeSpecRequest& request,
                            AddCompositeNodeSpecCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void GetBoardInfo(GetBoardInfoCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }
  void SetBoardInfo(SetBoardInfoRequest& request, SetBoardInfoCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void SetBootloaderInfo(SetBootloaderInfoRequest& request,
                         SetBootloaderInfoCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void RegisterSysSuspendCallback(RegisterSysSuspendCallbackRequest& request,
                                  RegisterSysSuspendCallbackCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_platform_bus::PlatformBus> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {}

  std::vector<fuchsia_hardware_platform_bus::Node>& nodes() { return nodes_; }

 private:
  std::vector<fuchsia_hardware_platform_bus::Node> nodes_;
};

class FakeCompositeNodeManager final
    : public fidl::Server<fuchsia_driver_framework::CompositeNodeManager> {
 public:
  void AddSpec(AddSpecRequest& request, AddSpecCompleter::Sync& completer) override {
    requests_.emplace_back(std::move(request));
    completer.Reply(zx::ok());
  }

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_driver_framework::CompositeNodeManager> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {}

  std::vector<AddSpecRequest> requests() { return requests_; }

 private:
  std::vector<AddSpecRequest> requests_;
};

class FakeNode final : public fidl::Server<fuchsia_driver_framework::Node> {
 public:
  void AddChild(AddChildRequest& request, AddChildCompleter::Sync& completer) override {
    requests_.push_back(std::make_shared<AddChildRequest>(std::move(request)));
    completer.Reply(zx::ok());
  }

  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_driver_framework::Node> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override {}

  std::vector<std::shared_ptr<AddChildRequest>>& requests() { return requests_; }

 private:
  std::vector<std::shared_ptr<AddChildRequest>> requests_;
};

class FakeEnvWrapper {
 public:
  void Bind(fdf::ServerEnd<fuchsia_hardware_platform_bus::PlatformBus> pbus_endpoints_server,
            fidl::ServerEnd<fuchsia_driver_framework::CompositeNodeManager> mgr_endpoints_server,
            fidl::ServerEnd<fuchsia_driver_framework::Node> node_endpoint_server);

  size_t pbus_node_size();

  size_t non_pbus_node_size();

  size_t mgr_requests_size();

  fidl::Request<fuchsia_driver_framework::CompositeNodeManager::AddSpec> mgr_requests_at(
      size_t index);

  fuchsia_hardware_platform_bus::Node pbus_nodes_at(size_t index);

  std::shared_ptr<fidl::Request<fuchsia_driver_framework::Node::AddChild>> non_pbus_nodes_at(
      size_t index);

 private:
  FakePlatformBus pbus_;
  FakeCompositeNodeManager mgr_;
  FakeNode node_;
};

class ManagerTestHelper {
 public:
  explicit ManagerTestHelper(std::string_view unused_tag) {}

  zx::result<> DoPublish(Manager& manager);

  async_patterns::TestDispatcherBound<FakeEnvWrapper>& env() { return env_; }

 private:
  fdf_testing::DriverRuntime runtime_;
  fdf_testing::ScopedGlobalLogger logger_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher = runtime_.StartBackgroundDispatcher();
  async_patterns::TestDispatcherBound<FakeEnvWrapper> env_{env_dispatcher->async_dispatcher(),
                                                           std::in_place};
  fidl::SyncClient<fuchsia_driver_framework::Node> node_;
  fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus> pbus_;
};

}  // namespace fdf_devicetree::testing

#endif  // LIB_DRIVER_DEVICETREE_MANAGER_MANAGER_TEST_HELPER_H_
