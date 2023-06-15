// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/misc/drivers/compat/device.h"

#include <fidl/fuchsia.component.runner/cpp/wire_types.h>
#include <fidl/fuchsia.device/cpp/markers.h>
#include <fidl/fuchsia.driver.framework/cpp/wire_test_base.h>
#include <fidl/test.placeholders/cpp/wire.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/symbols.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fidl/cpp/wire/connect_service.h>
#include <lib/fidl/cpp/wire/transaction.h>

#include <gtest/gtest.h>

#include "lib/ddk/binding_priv.h"
#include "lib/ddk/device.h"
#include "lib/ddk/driver.h"
#include "src/devices/misc/drivers/compat/driver.h"
#include "src/lib/storage/vfs/cpp/service.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}
namespace fio = fuchsia_io;
namespace frunner = fuchsia_component_runner;

class DeviceTest : public gtest::TestLoopFixture {
 public:
  void SetUp() override {
    TestLoopFixture::SetUp();

    auto svc = fidl::CreateEndpoints<fio::Directory>();
    ASSERT_EQ(ZX_OK, svc.status_value());
    auto ns = CreateNamespace(std::move(svc->client));
    ASSERT_EQ(ZX_OK, ns.status_value());

    auto logger = fdf::Logger::Create(*ns, dispatcher(), "test-logger", FUCHSIA_LOG_INFO, false);
    ASSERT_EQ(ZX_OK, logger.status_value());
    logger_ = std::shared_ptr<fdf::Logger>((*logger).release());
  }

 protected:
  std::shared_ptr<fdf::Logger> logger() { return logger_; }

 private:
  zx::result<fdf::Namespace> CreateNamespace(fidl::ClientEnd<fio::Directory> client_end) {
    fidl::Arena arena;
    fidl::VectorView<frunner::wire::ComponentNamespaceEntry> entries(arena, 1);
    entries[0] = frunner::wire::ComponentNamespaceEntry::Builder(arena)
                     .path("/svc")
                     .directory(std::move(client_end))
                     .Build();
    return fdf::Namespace::Create(entries);
  }

  std::shared_ptr<fdf::Logger> logger_;
};

TEST_F(DeviceTest, ConstructDevice) {
  fdf_testing::TestNode node("root", dispatcher());
  zx::result node_client = node.CreateNodeChannel();
  ASSERT_EQ(ZX_OK, node_client.status_value());

  // Create a device.
  zx_protocol_device_t ops{};
  compat::Device device(compat::kDefaultDevice, &ops, nullptr, std::nullopt, logger(),
                        dispatcher());
  device.Bind({std::move(node_client.value()), dispatcher()});

  // Test basic functions on the device.
  EXPECT_EQ(reinterpret_cast<uintptr_t>(&device), reinterpret_cast<uintptr_t>(device.ZxDevice()));
  EXPECT_STREQ("compat-device", device.Name());
  EXPECT_FALSE(device.HasChildren());

  device.Unbind();
  ASSERT_TRUE(RunLoopUntilIdle());
}

TEST_F(DeviceTest, AddChildDevice) {
  fdf_testing::TestNode node("root", dispatcher());
  zx::result node_client = node.CreateNodeChannel();
  ASSERT_EQ(ZX_OK, node_client.status_value());

  // Create a device.
  zx_protocol_device_t ops{};
  compat::Device parent(compat::kDefaultDevice, &ops, nullptr, std::nullopt, logger(),
                        dispatcher());
  parent.Bind({std::move(node_client.value()), dispatcher()});

  // Add a child device.
  device_add_args_t args{.name = "child"};
  zx_device_t* child = nullptr;
  ASSERT_EQ(ZX_OK, parent.Add(&args, &child));
  ASSERT_EQ(ZX_OK, child->CreateNode());
  EXPECT_NE(nullptr, child);
  EXPECT_STREQ("child", child->Name());
  EXPECT_TRUE(parent.HasChildren());

  // Ensure that AddChild was executed.
  ASSERT_TRUE(RunLoopUntilIdle());
}

TEST_F(DeviceTest, RemoveChildren) {
  fdf_testing::TestNode node("root", dispatcher());
  zx::result node_client = node.CreateNodeChannel();
  ASSERT_EQ(ZX_OK, node_client.status_value());

  // Create a device.
  zx_protocol_device_t ops{};
  compat::Device parent(compat::kDefaultDevice, &ops, nullptr, std::nullopt, logger(),
                        dispatcher());
  parent.Bind({std::move(node_client.value()), dispatcher()});
  parent.InitReply(ZX_OK);
  ASSERT_TRUE(RunLoopUntilIdle());

  // Add a child device.
  device_add_args_t args{.name = "child"};
  zx_device_t* child = nullptr;
  ASSERT_EQ(ZX_OK, parent.Add(&args, &child));
  EXPECT_NE(nullptr, child);
  EXPECT_STREQ("child", child->Name());
  EXPECT_TRUE(parent.HasChildren());

  // Ensure that AddChild was executed.
  ASSERT_TRUE(RunLoopUntilIdle());

  // Add a second child device.
  device_add_args_t args2{.name = "child2"};
  zx_device_t* child2 = nullptr;
  ASSERT_EQ(ZX_OK, parent.Add(&args2, &child2));
  EXPECT_NE(nullptr, child2);
  EXPECT_STREQ("child2", child2->Name());
  EXPECT_TRUE(parent.HasChildren());

  // Ensure that AddChild was executed.
  ASSERT_TRUE(RunLoopUntilIdle());

  // Call Remove children and check that the callback finished and the children
  // were removed.
  bool callback_finished = false;
  parent.executor().schedule_task(parent.RemoveChildren().and_then(
      [&callback_finished]() mutable { callback_finished = true; }));
  ASSERT_TRUE(RunLoopUntilIdle());
  ASSERT_TRUE(callback_finished);
  ASSERT_FALSE(parent.HasChildren());
}

TEST_F(DeviceTest, AddChildWithProtoPropAndProtoId) {
  fdf_testing::TestNode node("root", dispatcher());
  zx::result node_client = node.CreateNodeChannel();
  ASSERT_EQ(ZX_OK, node_client.status_value());

  // Create a device.
  zx_protocol_device_t ops{};
  compat::Device parent(compat::kDefaultDevice, &ops, nullptr, std::nullopt, logger(),
                        dispatcher());
  parent.Bind({std::move(node_client.value()), dispatcher()});

  // Add a child device.
  zx_device_prop_t prop{.id = BIND_PROTOCOL, .value = ZX_PROTOCOL_I2C};
  device_add_args_t args{
      .name = "child", .props = &prop, .prop_count = 1, .proto_id = ZX_PROTOCOL_BLOCK};
  zx_device_t* child = nullptr;
  ASSERT_EQ(ZX_OK, parent.Add(&args, &child));
  ASSERT_EQ(ZX_OK, child->CreateNode());

  EXPECT_NE(nullptr, child);
  EXPECT_STREQ("child", child->Name());
  EXPECT_TRUE(parent.HasChildren());

  ASSERT_TRUE(RunLoopUntilIdle());

  // Check the child was added with the right properties.
  ASSERT_EQ(node.children().count("child"), 1ul);
  const fdf_testing::TestNode& child_node = node.children().at("child");
  std::vector properties = child_node.GetProperties();
  ASSERT_EQ(1ul, properties.size());
  ASSERT_EQ(properties[0].key().int_value().value(), static_cast<uint32_t>(BIND_PROTOCOL));
  ASSERT_EQ(properties[0].value().int_value().value(), static_cast<uint32_t>(ZX_PROTOCOL_I2C));
}

TEST_F(DeviceTest, AddChildWithStringProps) {
  fdf_testing::TestNode node("root", dispatcher());
  zx::result node_client = node.CreateNodeChannel();
  ASSERT_EQ(ZX_OK, node_client.status_value());

  // Create a device.
  zx_protocol_device_t ops{};
  compat::Device parent(compat::kDefaultDevice, &ops, nullptr, std::nullopt, logger(),
                        dispatcher());
  parent.Bind({std::move(node_client.value()), dispatcher()});

  // Add a child device.
  zx_device_str_prop_t props[4] = {
      zx_device_str_prop_t{
          .key = "hello",
          .property_value = str_prop_int_val(1),
      },
      zx_device_str_prop_t{
          .key = "another",
          .property_value = str_prop_bool_val(true),
      },
      zx_device_str_prop_t{
          .key = "key",
          .property_value = str_prop_str_val("value"),
      },
      zx_device_str_prop_t{
          .key = "enum_key",
          .property_value = str_prop_enum_val("enum_value"),
      },
  };
  device_add_args_t args{.name = "child",
                         .str_props = props,
                         .str_prop_count = sizeof(props) / sizeof(props[0]),
                         .proto_id = ZX_PROTOCOL_BLOCK};
  zx_device_t* child = nullptr;
  ASSERT_EQ(ZX_OK, parent.Add(&args, &child));
  ASSERT_EQ(ZX_OK, child->CreateNode());
  EXPECT_NE(nullptr, child);
  EXPECT_STREQ("child", child->Name());
  EXPECT_TRUE(parent.HasChildren());

  ASSERT_TRUE(RunLoopUntilIdle());
  // Check the child was added with the right properties.
  ASSERT_EQ(node.children().count("child"), 1ul);
  const fdf_testing::TestNode& child_node = node.children().at("child");
  std::vector properties = child_node.GetProperties();
  ASSERT_EQ(5ul, properties.size());
  ASSERT_EQ(properties[0].key().string_value().value(), "hello");
  ASSERT_EQ(properties[0].value().int_value().value(), 1u);
  ASSERT_EQ(properties[1].key().string_value().value(), "another");
  ASSERT_EQ(properties[1].value().bool_value().value(), true);
  ASSERT_EQ(properties[2].key().string_value().value(), "key");
  ASSERT_EQ(properties[2].value().string_value().value(), "value");
  ASSERT_EQ(properties[3].key().string_value().value(), "enum_key");
  ASSERT_EQ(properties[3].value().string_value().value(), "enum_value");
}

TEST_F(DeviceTest, AddChildDeviceWithInit) {
  fdf_testing::TestNode node("root", dispatcher());
  zx::result node_client = node.CreateNodeChannel();
  ASSERT_EQ(ZX_OK, node_client.status_value());

  // Create a device.
  zx_protocol_device_t parent_ops{};
  compat::Device parent(compat::kDefaultDevice, &parent_ops, nullptr, std::nullopt, logger(),
                        dispatcher());
  parent.Bind({std::move(node_client.value()), dispatcher()});

  // Add a child device.
  bool child_ctx = false;
  static zx_protocol_device_t child_ops{
      .init = [](void* ctx) { *static_cast<bool*>(ctx) = true; },
  };
  device_add_args_t args{
      .name = "child",
      .ctx = &child_ctx,
      .ops = &child_ops,
  };
  zx_device_t* child = nullptr;
  ASSERT_EQ(ZX_OK, parent.Add(&args, &child));
  EXPECT_STREQ("child", child->Name());
  EXPECT_TRUE(parent.HasChildren());

  // Run the loop which should call the init op.
  ASSERT_TRUE(RunLoopUntilIdle());

  // Check that init promise hasn't finished yet.
  bool init_is_finished = false;
  child->executor().schedule_task(child->WaitForInitToComplete().and_then(
      [&init_is_finished]() mutable { init_is_finished = true; }));
  ASSERT_TRUE(RunLoopUntilIdle());
  EXPECT_FALSE(init_is_finished);

  // Reply to init. This shouldn't be marked as finished because the parent hasn't finished
  // initializing.
  device_init_reply(child, ZX_OK, nullptr);
  EXPECT_TRUE(RunLoopUntilIdle());
  ASSERT_FALSE(init_is_finished);

  // Parent finishes initializing.
  parent.InitReply(ZX_OK);
  EXPECT_TRUE(RunLoopUntilIdle());
  ASSERT_TRUE(init_is_finished);
}

TEST_F(DeviceTest, AddChildDeviceWithInitFailure) {
  fdf_testing::TestNode node("root", dispatcher());
  zx::result node_client = node.CreateNodeChannel();
  ASSERT_EQ(ZX_OK, node_client.status_value());

  // Create a device.
  zx_protocol_device_t parent_ops{};
  compat::Device parent(compat::kDefaultDevice, &parent_ops, nullptr, std::nullopt, logger(),
                        dispatcher());
  parent.Bind({std::move(node_client.value()), dispatcher()});

  // Add a child device.
  bool child_ctx = false;
  static zx_protocol_device_t child_ops{
      .init = [](void* ctx) { *static_cast<bool*>(ctx) = true; },
  };
  device_add_args_t args{
      .name = "child",
      .ctx = &child_ctx,
      .ops = &child_ops,
  };
  zx_device_t* child = nullptr;
  ASSERT_EQ(ZX_OK, parent.Add(&args, &child));
  EXPECT_STREQ("child", child->Name());
  EXPECT_TRUE(parent.HasChildren());

  // Run the loop which should call the init op.
  ASSERT_TRUE(RunLoopUntilIdle());

  // Reply to init with error.
  device_init_reply(child, ZX_ERR_BAD_STATE, nullptr);
  EXPECT_TRUE(RunLoopUntilIdle());

  // Parent init finishes.
  parent.InitReply(ZX_OK);
  EXPECT_TRUE(RunLoopUntilIdle());

  // Should not have a child since the init failed on the child.
  ASSERT_FALSE(parent.HasChildren());
}

TEST_F(DeviceTest, ParentInitFails) {
  fdf_testing::TestNode node("root", dispatcher());
  zx::result node_client = node.CreateNodeChannel();
  ASSERT_EQ(ZX_OK, node_client.status_value());

  // Create a device.
  zx_protocol_device_t parent_ops{};
  compat::Device parent(compat::kDefaultDevice, &parent_ops, nullptr, std::nullopt, logger(),
                        dispatcher());
  parent.Bind({std::move(node_client.value()), dispatcher()});
  parent.InitReply(ZX_OK);

  // Add child one.
  zx_protocol_device_t ops = {
      .init = [](void*) {},
  };
  device_add_args_t args_one{
      .name = "child-one",
      .ops = &ops,
  };
  zx_device_t* child_one = nullptr;
  ASSERT_EQ(ZX_OK, parent.Add(&args_one, &child_one));
  EXPECT_NE(nullptr, child_one);
  EXPECT_TRUE(parent.HasChildren());

  // Add child two.
  device_add_args_t args{
      .name = "child-two",
      .ops = &ops,
  };
  zx_device_t* child_two = nullptr;
  ASSERT_EQ(ZX_OK, child_one->Add(&args, &child_two));
  EXPECT_NE(nullptr, child_two);
  EXPECT_TRUE(child_one->HasChildren());

  // Run the loop which should call the init op.
  ASSERT_TRUE(RunLoopUntilIdle());

  // Finish the initialization with an error. Child_two should now be freed once it's finished
  // initializing.
  device_init_reply(child_one, ZX_ERR_INTERNAL, nullptr);

  EXPECT_TRUE(RunLoopUntilIdle());
  ASSERT_TRUE(child_one->HasChildren());

  device_init_reply(child_two, ZX_OK, nullptr);
  EXPECT_TRUE(RunLoopUntilIdle());
  ASSERT_FALSE(parent.HasChildren());
}

TEST_F(DeviceTest, AddAndRemoveChildDevice) {
  fdf_testing::TestNode node("root", dispatcher());
  zx::result node_client = node.CreateNodeChannel();
  ASSERT_EQ(ZX_OK, node_client.status_value());

  // Create a device.
  zx_protocol_device_t ops{};
  compat::Device parent(compat::kDefaultDevice, &ops, nullptr, std::nullopt, logger(),
                        dispatcher());
  parent.Bind({std::move(node_client.value()), dispatcher()});
  parent.InitReply(ZX_OK);

  // Add a child device.
  device_add_args_t args{.name = "child"};
  zx_device_t* child = nullptr;
  ASSERT_EQ(ZX_OK, parent.Add(&args, &child));
  EXPECT_NE(nullptr, child);
  EXPECT_STREQ("child", child->Name());
  EXPECT_TRUE(parent.HasChildren());

  // Remove the child device.
  child->Remove();
  ASSERT_TRUE(RunLoopUntilIdle());

  // Check that the related child device is removed from the parent device.
  EXPECT_FALSE(parent.HasChildren());
}

TEST_F(DeviceTest, AddChildToBindableDevice) {
  zx_protocol_device_t ops{};
  compat::Device parent(compat::kDefaultDevice, &ops, nullptr, std::nullopt, logger(),
                        dispatcher());
  parent.InitReply(ZX_OK);

  // Try to a child device.
  device_add_args_t args{.name = "child"};
  zx_device_t* child = nullptr;
  ASSERT_EQ(ZX_OK, parent.Add(&args, &child));

  // The parent does not have a Node, so child won't be able to create its own Node.
  ASSERT_EQ(ZX_ERR_NOT_SUPPORTED, child->CreateNode());
}

TEST_F(DeviceTest, GetProtocolFromDevice) {
  // Create a device without a get_protocol hook.
  zx_protocol_device_t ops{};
  compat::Device without(compat::kDefaultDevice, &ops, nullptr, std::nullopt, logger(),
                         dispatcher());
  ASSERT_EQ(ZX_ERR_NOT_SUPPORTED, without.GetProtocol(ZX_PROTOCOL_BLOCK, nullptr));

  // Create a device with a get_protocol hook.
  ops.get_protocol = [](void* ctx, uint32_t proto_id, void* protocol) {
    EXPECT_EQ(ZX_PROTOCOL_BLOCK, proto_id);
    return ZX_OK;
  };
  compat::Device with(compat::kDefaultDevice, &ops, nullptr, std::nullopt, logger(), dispatcher());
  ASSERT_EQ(ZX_OK, with.GetProtocol(ZX_PROTOCOL_BLOCK, nullptr));
}

TEST_F(DeviceTest, DeviceMetadata) {
  // Create a device.
  zx_protocol_device_t ops{};
  compat::Device device(compat::kDefaultDevice, &ops, nullptr, std::nullopt, logger(),
                        dispatcher());

  // Add metadata to the device.
  const uint64_t metadata = 0xAABBCCDDEEFF0011;
  zx_status_t status = device.AddMetadata(DEVICE_METADATA_PRIVATE, &metadata, sizeof(metadata));
  ASSERT_EQ(ZX_OK, status);

  // Add the same metadata again.
  status = device.AddMetadata(DEVICE_METADATA_PRIVATE, &metadata, sizeof(metadata));
  ASSERT_EQ(ZX_OK, status);

  // Check the metadata size.
  size_t size = 0;
  status = device.GetMetadataSize(DEVICE_METADATA_PRIVATE, &size);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_EQ(sizeof(metadata), size);

  // Check the metadata size for missing metadata.
  status = device.GetMetadataSize(DEVICE_METADATA_BOARD_PRIVATE, &size);
  ASSERT_EQ(ZX_ERR_NOT_FOUND, status);

  // Get the metadata.
  uint64_t found = 0;
  size_t found_size = 0;
  status = device.GetMetadata(DEVICE_METADATA_PRIVATE, &found, sizeof(found), &found_size);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_EQ(metadata, found);
  EXPECT_EQ(sizeof(metadata), found_size);

  // Get the metadata for missing metadata.
  status = device.GetMetadata(DEVICE_METADATA_BOARD_PRIVATE, &found, sizeof(found), &found_size);
  ASSERT_EQ(ZX_ERR_NOT_FOUND, status);
}

TEST_F(DeviceTest, DeviceFragmentMetadata) {
  // Create a device.
  zx_protocol_device_t ops{};
  compat::Device device(compat::kDefaultDevice, &ops, nullptr, std::nullopt, logger(),
                        dispatcher());

  // Add metadata to the device.
  const uint64_t metadata = 0xAABBCCDDEEFF0011;
  zx_status_t status = device.AddMetadata(DEVICE_METADATA_PRIVATE, &metadata, sizeof(metadata));
  ASSERT_EQ(ZX_OK, status);

  // Get the metadata.
  uint64_t found = 0;
  size_t found_size = 0;
  status = device_get_fragment_metadata(device.ZxDevice(), "fragment-name", DEVICE_METADATA_PRIVATE,
                                        &found, sizeof(found), &found_size);
  ASSERT_EQ(ZX_OK, status);
  EXPECT_EQ(metadata, found);
  EXPECT_EQ(sizeof(metadata), found_size);
}

TEST_F(DeviceTest, GetFragmentProtocolFromDeviceNoDriver) {
  // Create a device.
  zx_protocol_device_t ops{};
  compat::Device with(compat::kDefaultDevice, &ops, nullptr, std::nullopt, logger(), dispatcher());
  std::vector<std::string> fragments;
  fragments.push_back("fragment-name");
  with.set_fragments(std::move(fragments));

  struct GenericProtocol {
    const void* ops;
    void* ctx;
  } proto;
  ASSERT_EQ(ZX_ERR_BAD_STATE, device_get_fragment_protocol(with.ZxDevice(), "fragment-name",
                                                           ZX_PROTOCOL_BLOCK, &proto));
}

TEST_F(DeviceTest, GetTopologicalPath) {
  auto endpoints = fidl::CreateEndpoints<fdf::Node>();

  // Create a device.
  zx_protocol_device_t ops{};
  compat::Device device(compat::kDefaultDevice, &ops, nullptr, std::nullopt, logger(),
                        dispatcher());
  device.Bind({std::move(endpoints->client), dispatcher()});

  // The root device doesn't have a valid topological path, so we add a child.
  zx_device_t* second_device;
  device_add_args_t args{
      .name = "second-device",
  };
  device.Add(&args, &second_device);

  auto dev_endpoints = fidl::CreateEndpoints<fuchsia_device::Controller>();
  ASSERT_EQ(ZX_OK, endpoints.status_value());

  fidl::BindServer(test_loop().dispatcher(), std::move(dev_endpoints->server), second_device);

  fidl::WireClient<fuchsia_device::Controller> client;
  client.Bind(std::move(dev_endpoints->client), test_loop().dispatcher());

  bool callback_called = false;
  client->GetTopologicalPath().Then(
      [&callback_called](
          fidl::WireUnownedResult<fuchsia_device::Controller::GetTopologicalPath>& result) {
        if (!result.ok()) {
          FAIL() << result.error();
          return;
        }
        ASSERT_TRUE(result->is_ok());
        std::string path(result->value()->path.data(), result->value()->path.size());
        EXPECT_STREQ("/dev/second-device", path.data());
        callback_called = true;
      });

  ASSERT_TRUE(test_loop().RunUntilIdle());
  ASSERT_TRUE(callback_called);
}

TEST_F(DeviceTest, SetPerformanceState) {
  auto endpoints = fidl::CreateEndpoints<fdf::Node>();

  // Create a device.
  zx_protocol_device_t ops{
      .set_performance_state = [](void* ctx, uint32_t state, uint32_t* out_state) -> zx_status_t {
        *out_state = state;
        return ZX_OK;
      }};
  compat::Device device(compat::kDefaultDevice, &ops, nullptr, std::nullopt, logger(),
                        dispatcher());
  device.Bind({std::move(endpoints->client), dispatcher()});

  auto dev_endpoints = fidl::CreateEndpoints<fuchsia_device::Controller>();
  ASSERT_EQ(ZX_OK, endpoints.status_value());

  fidl::BindServer(test_loop().dispatcher(), std::move(dev_endpoints->server), &device);

  fidl::WireClient<fuchsia_device::Controller> client;
  client.Bind(std::move(dev_endpoints->client), test_loop().dispatcher());

  bool callback_called = false;
  const uint32_t kState = 5;
  client->SetPerformanceState(kState).Then(
      [&callback_called,
       kState](fidl::WireUnownedResult<fuchsia_device::Controller::SetPerformanceState>& result) {
        if (!result.ok()) {
          FAIL() << result.error();
          return;
        }
        EXPECT_EQ(ZX_OK, result->status);
        EXPECT_EQ(kState, result->out_state);
        callback_called = true;
      });

  ASSERT_TRUE(test_loop().RunUntilIdle());
  ASSERT_TRUE(callback_called);
}

TEST_F(DeviceTest, SetAndGetMinDriverLogSeverity) {
  auto endpoints = fidl::CreateEndpoints<fdf::Node>();

  // Create a device.
  zx_protocol_device_t ops{};
  compat::Device device(compat::kDefaultDevice, &ops, nullptr, std::nullopt, logger(),
                        dispatcher());
  device.Bind({std::move(endpoints->client), dispatcher()});

  auto dev_endpoints = fidl::CreateEndpoints<fuchsia_device::Controller>();
  ASSERT_EQ(ZX_OK, endpoints.status_value());

  fidl::BindServer(test_loop().dispatcher(), std::move(dev_endpoints->server), &device);

  fidl::WireClient<fuchsia_device::Controller> client;
  client.Bind(std::move(dev_endpoints->client), test_loop().dispatcher());

  bool callback_called = false;
  client->SetMinDriverLogSeverity(fuchsia_logger::wire::LogLevelFilter::kError)
      .Then([&client, &callback_called](
                fidl::WireUnownedResult<fuchsia_device::Controller::SetMinDriverLogSeverity>&
                    result) {
        if (!result.ok()) {
          FAIL() << result.error();
          return;
        }
        ASSERT_EQ(ZX_OK, result->status);
        client->GetMinDriverLogSeverity().Then(
            [&client, &callback_called](
                fidl::WireUnownedResult<fuchsia_device::Controller::GetMinDriverLogSeverity>&
                    result) {
              if (!result.ok()) {
                FAIL() << result.error();
                return;
              }
              ASSERT_EQ(ZX_OK, result->status);
              ASSERT_EQ(fuchsia_logger::wire::LogLevelFilter::kError, result->severity);

              // We set and get again because we cannot confirm if the first
              // call to set actually worked. The min driver log severity that
              // the first get compares to may have been unluckily the logger's
              // initial min driver log severity.
              client->SetMinDriverLogSeverity(fuchsia_logger::wire::LogLevelFilter::kInfo)
                  .Then([&client, &callback_called](
                            fidl::WireUnownedResult<
                                fuchsia_device::Controller::SetMinDriverLogSeverity>& result) {
                    if (!result.ok()) {
                      FAIL() << result.error();
                      return;
                    }
                    ASSERT_EQ(ZX_OK, result->status);

                    client->GetMinDriverLogSeverity().Then(
                        [&callback_called](
                            fidl::WireUnownedResult<
                                fuchsia_device::Controller::GetMinDriverLogSeverity>& result) {
                          if (!result.ok()) {
                            FAIL() << result.error();
                            return;
                          }
                          ASSERT_EQ(ZX_OK, result->status);
                          ASSERT_EQ(fuchsia_logger::wire::LogLevelFilter::kInfo, result->severity);
                          callback_called = true;
                        });
                  });
            });
      });

  ASSERT_TRUE(test_loop().RunUntilIdle());
  ASSERT_TRUE(callback_called);
}

TEST_F(DeviceTest, TestBind) {
  fdf_testing::TestNode node("root", dispatcher());
  zx::result node_client = node.CreateNodeChannel();
  ASSERT_EQ(ZX_OK, node_client.status_value());

  // Create a device.
  zx_protocol_device_t ops{};
  compat::Device device(compat::kDefaultDevice, &ops, nullptr, std::nullopt, logger(),
                        dispatcher());
  device.Bind({std::move(node_client.value()), dispatcher()});

  zx_device_t* second_device;
  device_add_args_t args{
      .name = "second-device",
  };
  ASSERT_EQ(ZX_OK, device.Add(&args, &second_device));
  ASSERT_EQ(ZX_OK, second_device->CreateNode());

  auto dev_endpoints = fidl::CreateEndpoints<fuchsia_device::Controller>();
  ASSERT_EQ(ZX_OK, dev_endpoints.status_value());

  fidl::BindServer(test_loop().dispatcher(), std::move(dev_endpoints->server), second_device);
  fidl::WireClient client{std::move(dev_endpoints->client), test_loop().dispatcher()};

  bool callback_called = false;
  client->Bind("gpt.so").Then(
      [&callback_called](fidl::WireUnownedResult<fuchsia_device::Controller::Bind>& result) {
        if (!result.ok()) {
          FAIL() << result.error();
          return;
        }
        ASSERT_TRUE(result->is_ok())
            << "Bind result failed " << zx_status_get_string(result->error_value());
        callback_called = true;
      });

  ASSERT_TRUE(test_loop().RunUntilIdle());
  ASSERT_TRUE(callback_called);
  ASSERT_FALSE(static_cast<devfs_fidl::DeviceInterface*>(second_device)->IsUnbound());

  ASSERT_EQ(node.children().count("second-device"), 1ul);
  const fdf_testing::TestNode& child_node = node.children().at("second-device");
  std::vector bind_data = child_node.GetBindData();
  ASSERT_EQ(1ul, bind_data.size());
  ASSERT_FALSE(bind_data[0].force_rebind);
  ASSERT_EQ(bind_data[0].driver_url_suffix, "gpt.so");
}

TEST_F(DeviceTest, TestBindAlreadyBound) {
  fdf_testing::TestNode node("root", dispatcher());
  zx::result node_client = node.CreateNodeChannel();
  ASSERT_EQ(ZX_OK, node_client.status_value());

  // Create a device.
  zx_protocol_device_t ops{};
  compat::Device device(compat::kDefaultDevice, &ops, nullptr, std::nullopt, logger(),
                        dispatcher());
  device.Bind({std::move(node_client.value()), dispatcher()});

  zx_device_t* second_device;
  device_add_args_t args{
      .name = "second-device",
      .flags = DEVICE_ADD_NON_BINDABLE,
  };
  device.Add(&args, &second_device);
  ASSERT_EQ(ZX_OK, second_device->CreateNode());

  // create another device.
  zx_device_t* third_device;
  second_device->Add(&args, &third_device);
  ASSERT_EQ(ZX_OK, third_device->CreateNode());

  auto dev_endpoints = fidl::CreateEndpoints<fuchsia_device::Controller>();
  ASSERT_EQ(ZX_OK, dev_endpoints.status_value());

  fidl::BindServer(test_loop().dispatcher(), std::move(dev_endpoints->server), second_device);
  fidl::WireClient client{std::move(dev_endpoints->client), test_loop().dispatcher()};

  bool got_reply = false;
  client->Bind("gpt.so").Then(
      [&got_reply](fidl::WireUnownedResult<fuchsia_device::Controller::Bind>& result) {
        if (!result.ok()) {
          FAIL() << "Bind failed: " << result.error();
          return;
        }
        ASSERT_TRUE(result->is_error());
        ASSERT_EQ(ZX_ERR_ALREADY_BOUND, result->error_value());
        got_reply = true;
      });

  ASSERT_TRUE(test_loop().RunUntilIdle());
  ASSERT_TRUE(got_reply);
}

TEST_F(DeviceTest, TestRebind) {
  fdf_testing::TestNode node("root", dispatcher());
  zx::result node_client = node.CreateNodeChannel();
  ASSERT_EQ(ZX_OK, node_client.status_value());

  // Create a device.
  zx_protocol_device_t ops{};
  compat::Device device(compat::kDefaultDevice, &ops, nullptr, std::nullopt, logger(),
                        dispatcher());
  device.Bind({std::move(node_client.value()), dispatcher()});

  zx_device_t* second_device;
  device_add_args_t args{
      .name = "second-device",
  };
  ASSERT_EQ(ZX_OK, device.Add(&args, &second_device));
  ASSERT_EQ(ZX_OK, second_device->CreateNode());

  bool got_reply = false;

  auto dev_endpoints = fidl::CreateEndpoints<fuchsia_device::Controller>();
  ASSERT_EQ(ZX_OK, dev_endpoints.status_value());

  fidl::BindServer(test_loop().dispatcher(), std::move(dev_endpoints->server), second_device);
  fidl::WireClient client{std::move(dev_endpoints->client), test_loop().dispatcher()};

  client->Rebind("gpt.so").Then(
      [&got_reply](fidl::WireUnownedResult<fuchsia_device::Controller::Rebind>& result) {
        if (!result.ok()) {
          FAIL() << "Rebind failed: " << result.error();
          return;
        }
        ASSERT_TRUE(result->is_ok())
            << "Rebind failed " << zx_status_get_string(result->error_value());
        got_reply = true;
      });

  ASSERT_TRUE(test_loop().RunUntilIdle());
  ASSERT_TRUE(got_reply);

  const fdf_testing::TestNode& child_node = node.children().at("second-device");
  std::vector bind_data = child_node.GetBindData();
  ASSERT_EQ(1ul, bind_data.size());
  ASSERT_TRUE(bind_data[0].force_rebind);
  ASSERT_EQ(bind_data[0].driver_url_suffix, "gpt.so");
}

TEST_F(DeviceTest, CreateNodeProperties) {
  fidl::Arena<512> arena;
  auto logger = std::make_unique<fdf::Logger>("", 0, zx::socket(),
                                              fidl::WireClient<fuchsia_logger::LogSink>());
  device_add_args_t args = {};

  zx_device_prop_t prop;
  prop.id = 11;
  prop.value = 2;
  args.props = &prop;
  args.prop_count = 1;

  zx_device_str_prop_t str_prop;
  str_prop.key = "test";
  str_prop.property_value = str_prop_int_val(5);
  args.str_props = &str_prop;
  args.str_prop_count = 1;

  args.proto_id = 10;

  const char* service_offer = "fuchsia.hardware.i2c.Service";
  args.fidl_service_offers = &service_offer;
  args.fidl_service_offer_count = 1;

  const char* runtime_offer = "fuchsia.hardware.gpio.Service";
  args.runtime_service_offers = &runtime_offer;
  args.runtime_service_offer_count = 1;

  auto properties = compat::CreateProperties(arena, *logger, &args);

  ASSERT_EQ(7ul, properties.size());

  EXPECT_EQ(11u, properties[0].key.int_value());
  EXPECT_EQ(2u, properties[0].value.int_value());

  EXPECT_EQ("test", properties[1].key.string_value().get());
  EXPECT_EQ(5u, properties[1].value.int_value());

  EXPECT_EQ("fuchsia.hardware.i2c.Service", properties[2].key.string_value().get());
  EXPECT_EQ("fuchsia.hardware.i2c.Service.ZirconTransport",
            properties[2].value.string_value().get());

  EXPECT_EQ(static_cast<uint32_t>(BIND_FIDL_PROTOCOL), properties[3].key.int_value());
  EXPECT_EQ(3u, properties[3].value.int_value());

  EXPECT_EQ("fuchsia.hardware.gpio.Service", properties[4].key.string_value().get());
  EXPECT_EQ("fuchsia.hardware.gpio.Service.DriverTransport",
            properties[4].value.string_value().get());

  EXPECT_EQ(static_cast<uint32_t>(BIND_FIDL_PROTOCOL), properties[5].key.int_value());
  EXPECT_EQ(26u, properties[5].value.int_value());

  EXPECT_EQ(static_cast<uint32_t>(BIND_PROTOCOL), properties[6].key.int_value());
  EXPECT_EQ(10u, properties[6].value.int_value());
}
