// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.device/cpp/wire_test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>

#include <zxtest/zxtest.h>

#include "src/devices/lib/fidl/device_server.h"

namespace {

class TestInterface : public devfs_fidl::DeviceInterface {
 public:
  void LogError(const char* error) override { printf("%s\n", error); }
  bool IsUnbound() override { return false; }
  bool MessageOp(fidl::IncomingHeaderAndMessage msg, device_fidl_txn_t txn) override {
    return false;
  }

  void GetCurrentPerformanceState(GetCurrentPerformanceStateCompleter::Sync& completer) override {
    completer.Reply(1);
  }
  void ConnectToDeviceFidl(fuchsia_device::wire::ControllerConnectToDeviceFidlRequest* request,
                           ConnectToDeviceFidlCompleter::Sync& completer) override {
    ZX_PANIC("Unimplemented");
  }
  void ConnectToController(fuchsia_device::wire::ControllerConnectToControllerRequest* request,
                           ConnectToControllerCompleter::Sync& completer) override {
    ZX_PANIC("Unimplemented");
  }
  void Bind(fuchsia_device::wire::ControllerBindRequest* request,
            BindCompleter::Sync& completer) override {
    ZX_PANIC("Unimplemented");
  }
  void Rebind(fuchsia_device::wire::ControllerRebindRequest* request,
              RebindCompleter::Sync& completer) override {
    ZX_PANIC("Unimplemented");
  }
  void UnbindChildren(UnbindChildrenCompleter::Sync& completer) override {
    ZX_PANIC("Unimplemented");
  }
  void ScheduleUnbind(ScheduleUnbindCompleter::Sync& completer) override {
    ZX_PANIC("Unimplemented");
  }
  void GetTopologicalPath(GetTopologicalPathCompleter::Sync& completer) override {
    ZX_PANIC("Unimplemented");
  }
  void GetMinDriverLogSeverity(GetMinDriverLogSeverityCompleter::Sync& completer) override {
    ZX_PANIC("Unimplemented");
  }
  void SetMinDriverLogSeverity(
      fuchsia_device::wire::ControllerSetMinDriverLogSeverityRequest* request,
      SetMinDriverLogSeverityCompleter::Sync& completer) override {
    ZX_PANIC("Unimplemented");
  }
  void SetPerformanceState(fuchsia_device::wire::ControllerSetPerformanceStateRequest* request,
                           SetPerformanceStateCompleter::Sync& completer) override {
    ZX_PANIC("Unimplemented");
  }
};

class MultiplexTest : public zxtest::TestWithParam<std::tuple<bool, bool>> {
 public:
  struct Param {
    explicit Param(ParamType info)
        : include_node(std::get<0>(info)), include_controller(std::get<1>(info)) {}
    const bool include_node;
    const bool include_controller;
  };
};

TEST_P(MultiplexTest, CheckUnimplemented) {
  const Param param(GetParam());
  bool include_node = param.include_node;
  bool include_controller = param.include_controller;

  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  TestInterface interface;
  devfs_fidl::DeviceServer server{interface, loop.dispatcher()};
  {
    zx::result node = fidl::CreateEndpoints<fuchsia_io::Node>();
    ASSERT_OK(node);
    server.ServeMultiplexed(node->server.TakeChannel(), include_node, include_controller);

    fidl::WireClient node_client{std::move(node->client), loop.dispatcher()};
    bool did_run = false;
    node_client->Query().Then(
        [&did_run, include_node](fidl::WireUnownedResult<fuchsia_io::Node::Query>& result) {
          did_run = true;
          if (include_node) {
            ASSERT_OK(result.status());
          } else {
            ASSERT_STATUS(result.status(), ZX_ERR_PEER_CLOSED);
          }
        });
    ASSERT_OK(loop.RunUntilIdle());
    ASSERT_TRUE(did_run);
  }

  {
    zx::result controller = fidl::CreateEndpoints<fuchsia_device::Controller>();
    ASSERT_OK(controller);
    server.ServeMultiplexed(controller->server.TakeChannel(), include_node, include_controller);

    bool did_run = false;
    fidl::WireClient controller_client{std::move(controller->client), loop.dispatcher()};
    controller_client->GetCurrentPerformanceState().Then(
        [&did_run, include_controller](
            fidl::WireUnownedResult<fuchsia_device::Controller::GetCurrentPerformanceState>&
                result) {
          did_run = true;
          if (include_controller) {
            ASSERT_OK(result.status());
          } else {
            ASSERT_STATUS(result.status(), ZX_ERR_PEER_CLOSED);
          }
        });
    ASSERT_OK(loop.RunUntilIdle());
    ASSERT_TRUE(did_run);
  }

  bool did_run = false;
  server.CloseAllConnections([&did_run]() { did_run = true; });
  ASSERT_OK(loop.RunUntilIdle());
  ASSERT_TRUE(did_run);
}

INSTANTIATE_TEST_SUITE_P(DevfsFidl, MultiplexTest, zxtest::Combine(zxtest::Bool(), zxtest::Bool()),
                         [](const zxtest::TestParamInfo<MultiplexTest::ParamType>& info) {
                           const MultiplexTest::Param param(info.param);
                           std::string test_suffix;
                           if (param.include_controller) {
                             test_suffix += "WithController";
                           }
                           if (param.include_node) {
                             test_suffix += "WithNode";
                           }
                           return test_suffix;
                         });

}  // namespace
