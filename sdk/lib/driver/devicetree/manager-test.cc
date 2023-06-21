// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/driver/devicetree/manager.h"

#include <fcntl.h>
#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <lib/driver/legacy-bind-constants/legacy-bind-constants.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/runtime/testing/loop_fixture/test_loop_fixture.h>
#include <lib/syslog/cpp/macros.h>

#include <memory>
#include <sstream>
#include <unordered_set>

#include <bind/fuchsia/devicetree/cpp/bind.h>
#include <bind/fuchsia/platform/cpp/bind.h>
#include <gtest/gtest.h>

#include "sdk/lib/driver/devicetree/test-data/basic-properties.h"
#include "sdk/lib/driver/devicetree/visitor.h"
#include "sdk/lib/driver/devicetree/visitors/default.h"

namespace fdf_devicetree {
namespace {

std::string DebugStringifyProperty(const fuchsia_driver_framework::NodeProperty& prop) {
  std::stringstream ret;
  ret << "Key=";

  switch (prop.key().Which()) {
    using Tag = fuchsia_driver_framework::NodePropertyKey::Tag;
    case Tag::kIntValue:
      ret << "Int{" << prop.key().int_value().value() << "}";
      break;
    case Tag::kStringValue:
      ret << "Str{" << prop.key().string_value().value() << "}";
      break;
    default:
      ret << "Unknown{" << static_cast<int>(prop.key().Which()) << "}";
      break;
  }

  ret << " Value=";
  switch (prop.value().Which()) {
    using Tag = fuchsia_driver_framework::NodePropertyValue::Tag;
    case Tag::kBoolValue:
      ret << "Bool{" << prop.value().bool_value().value() << "}";
      break;
    case Tag::kEnumValue:
      ret << "Enum{" << prop.value().enum_value().value() << "}";
      break;
    case Tag::kIntValue:
      ret << "Int{" << prop.value().int_value().value() << "}";
      break;
    case Tag::kStringValue:
      ret << "String{" << prop.value().string_value().value() << "}";
      break;
    default:
      ret << "Unknown{" << static_cast<int>(prop.value().Which()) << "}";
      break;
  }

  return ret.str();
}

void AssertHasProperties(std::vector<fuchsia_driver_framework::NodeProperty> expected,
                         const fuchsia_driver_framework::ParentSpec& node) {
  for (auto& property : node.properties()) {
    auto iter = std::find(expected.begin(), expected.end(), property);
    EXPECT_NE(expected.end(), iter) << "Unexpected property: " << DebugStringifyProperty(property);
    if (iter != expected.end()) {
      expected.erase(iter);
    }
  }

  ASSERT_TRUE(expected.empty());
}

class FakePlatformBus final : public fdf::Server<fuchsia_hardware_platform_bus::PlatformBus> {
 public:
  void NodeAdd(NodeAddRequest& request, NodeAddCompleter::Sync& completer) override {
    nodes_.emplace_back(std::move(request.node()));
    completer.Reply(zx::ok());
  }
  void ProtocolNodeAdd(ProtocolNodeAddRequest& request,
                       ProtocolNodeAddCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }
  void RegisterProtocol(RegisterProtocolRequest& request,
                        RegisterProtocolCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
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
  void AddComposite(AddCompositeRequest& request, AddCompositeCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }
  void AddCompositeImplicitPbusFragment(
      AddCompositeImplicitPbusFragmentRequest& request,
      AddCompositeImplicitPbusFragmentCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }
  void RegisterSysSuspendCallback(RegisterSysSuspendCallbackRequest& request,
                                  RegisterSysSuspendCallbackCompleter::Sync& completer) override {
    completer.Reply(zx::error(ZX_ERR_NOT_SUPPORTED));
  }

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

  std::vector<AddSpecRequest> requests() { return requests_; }

 private:
  std::vector<AddSpecRequest> requests_;
};

class ManagerTest : public gtest::DriverTestLoopFixture {
 public:
  ManagerTest() { ConnectLogger(); }

  ~ManagerTest() { fdf::Logger::SetGlobalInstance(nullptr); }

  void ConnectLogger() {
    zx::socket client_end, server_end;
    zx_status_t status = zx::socket::create(ZX_SOCKET_DATAGRAM, &client_end, &server_end);
    ASSERT_EQ(status, ZX_OK);

    auto connect_result = component::Connect<fuchsia_logger::LogSink>();
    ASSERT_FALSE(connect_result.is_error());

    fidl::WireSyncClient<fuchsia_logger::LogSink> log_sink;
    log_sink.Bind(std::move(*connect_result));
    auto sink_result = log_sink->ConnectStructured(std::move(server_end));
    ASSERT_TRUE(sink_result.ok());

    logger_ = std::make_unique<fdf::Logger>("ManagerTest", 0, std::move(client_end),
                                            fidl::WireClient<fuchsia_logger::LogSink>());
    fdf::Logger::SetGlobalInstance(logger_.get());
  }

  // Load the file |name| into a vector and return it.
  static std::vector<uint8_t> LoadTestBlob(const char* name) {
    int fd = open(name, O_RDONLY);
    if (fd < 0) {
      FX_LOGS(ERROR) << "Open failed: " << strerror(errno);
      return {};
    }

    struct stat stat_out;
    if (fstat(fd, &stat_out) < 0) {
      FX_LOGS(ERROR) << "fstat failed: " << strerror(errno);
      return {};
    }

    std::vector<uint8_t> vec(stat_out.st_size);
    ssize_t bytes_read = read(fd, vec.data(), stat_out.st_size);
    if (bytes_read < 0) {
      FX_LOGS(ERROR) << "read failed: " << strerror(errno);
      return {};
    }
    vec.resize(bytes_read);
    return vec;
  }

  void DoPublish(Manager& manager) {
    auto pbus_endpoints = fdf::CreateEndpoints<fuchsia_hardware_platform_bus::PlatformBus>();
    ASSERT_EQ(ZX_OK, pbus_endpoints.status_value());
    auto mgr_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::CompositeNodeManager>();
    ASSERT_EQ(ZX_OK, mgr_endpoints.status_value());
    auto node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
    ASSERT_EQ(ZX_OK, node_endpoints.status_value());
    node_.Bind(std::move(node_endpoints->client));

    RunOnDispatcher([&]() {
      fdf::BindServer(fdf::Dispatcher::GetCurrent()->get(), std::move(pbus_endpoints->server),
                      &pbus_);
      fidl::BindServer(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                       std::move(mgr_endpoints->server), &mgr_);
    });

    ASSERT_EQ(
        ZX_OK,
        manager.PublishDevices(std::move(pbus_endpoints->client), std::move(mgr_endpoints->client))
            .status_value());
  }

  FakePlatformBus pbus_;
  FakeCompositeNodeManager mgr_;
  fidl::SyncClient<fuchsia_driver_framework::Node> node_;
  std::unique_ptr<fdf::Logger> logger_;
};

TEST_F(ManagerTest, TestFindsNodes) {
  Manager manager(LoadTestBlob("/pkg/test-data/simple.dtb"));
  class EmptyVisitor : public Visitor {
   public:
    zx::result<> Visit(Node& node, const devicetree::PropertyDecoder& decoder) override {
      return zx::ok();
    }
  };
  EmptyVisitor visitor;
  ASSERT_EQ(ZX_OK, manager.Walk(visitor).status_value());
  ASSERT_EQ(3lu, manager.nodes().size());

  // Root node is always first, and has no name.
  Node* node = manager.nodes()[0].get();
  ASSERT_STREQ("", node->name().data());

  // example-device node should be next.
  node = manager.nodes()[1].get();
  ASSERT_STREQ("example-device", node->name().data());

  // another-device should be last.
  node = manager.nodes()[2].get();
  ASSERT_STREQ("another-device", node->name().data());
}

TEST_F(ManagerTest, TestPropertyCallback) {
  Manager manager(LoadTestBlob("/pkg/test-data/simple.dtb"));
  class TestVisitor : public Visitor {
   public:
    zx::result<> Visit(Node& node, const devicetree::PropertyDecoder& decoder) override {
      for (auto& [name, _] : node.properties()) {
        if (node.name() == "example-device") {
          auto iter = expected.find(std::string(name));
          EXPECT_NE(expected.end(), iter) << "Property " << name << " was unexpected.";
          if (iter != expected.end()) {
            expected.erase(iter);
          }
        }
      }
      return zx::ok();
    }

    std::unordered_set<std::string> expected{
        "compatible",
        "phandle",
    };
  };

  TestVisitor visitor;
  ASSERT_EQ(ZX_OK, manager.Walk(visitor).status_value());
  EXPECT_EQ(0lu, visitor.expected.size());
}

TEST_F(ManagerTest, TestPublishesSimpleNode) {
  Manager manager(LoadTestBlob("/pkg/test-data/simple.dtb"));
  ASSERT_EQ(ZX_OK, manager.Walk(manager.default_visitor()).status_value());

  DoPublish(manager);
  ASSERT_EQ(2lu, pbus_.nodes().size());

  ASSERT_EQ(2lu, mgr_.requests().size());

  auto composite_node_spec = mgr_.requests()[1];
  ASSERT_TRUE(composite_node_spec.parents().has_value());
  ASSERT_TRUE(composite_node_spec.name().has_value());
  EXPECT_NE(nullptr, strstr("example-device", composite_node_spec.name()->data()));
  // First node is the primary node. In this case, it should be the platform device.
  ASSERT_FALSE(composite_node_spec.parents()->empty());

  auto& pbus_node = composite_node_spec.parents().value()[0];
  AssertHasProperties(
      {{{
           .key = fuchsia_driver_framework::NodePropertyKey::WithStringValue(
               bind_fuchsia_devicetree::FIRST_COMPATIBLE),
           .value = fuchsia_driver_framework::NodePropertyValue::WithStringValue(
               "fuchsia,sample-device"),
       }},
       {{
           .key = fuchsia_driver_framework::NodePropertyKey::WithIntValue(BIND_PROTOCOL),
           .value = fuchsia_driver_framework::NodePropertyValue::WithIntValue(
               bind_fuchsia_platform::BIND_PROTOCOL_DEVICE),
       }}},
      pbus_node);
}

TEST_F(ManagerTest, TestMmioProperties) {
  Manager manager(LoadTestBlob("/pkg/test-data/basic-properties.dtb"));
  ASSERT_EQ(ZX_OK, manager.Walk(manager.default_visitor()).status_value());

  DoPublish(manager);

  ASSERT_EQ(2lu, pbus_.nodes().size());
  // First node is devicetree root. Second one is the sample-device. Check MMIO of sample-device.
  auto mmio = pbus_.nodes()[1].mmio();

  // Test MMIO properties.
  ASSERT_TRUE(mmio);
  ASSERT_EQ(3lu, mmio->size());
  ASSERT_EQ(TEST_REG_A_BASE, *(*mmio)[0].base());
  ASSERT_EQ(static_cast<uint64_t>(TEST_REG_A_LENGTH), *(*mmio)[0].length());
  ASSERT_EQ((uint64_t)TEST_REG_B_BASE_WORD0 << 32 | TEST_REG_B_BASE_WORD1, *(*mmio)[1].base());
  ASSERT_EQ((uint64_t)TEST_REG_B_LENGTH_WORD0 << 32 | TEST_REG_B_LENGTH_WORD1,
            *(*mmio)[1].length());
  ASSERT_EQ((uint64_t)TEST_REG_C_BASE_WORD0 << 32 | TEST_REG_C_BASE_WORD1, *(*mmio)[2].base());
  ASSERT_EQ((uint64_t)TEST_REG_C_LENGTH_WORD0 << 32 | TEST_REG_C_LENGTH_WORD1,
            *(*mmio)[2].length());
}

}  // namespace
}  // namespace fdf_devicetree
