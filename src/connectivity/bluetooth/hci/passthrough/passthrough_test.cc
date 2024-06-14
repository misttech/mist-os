// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "passthrough.h"

#include <lib/async-loop/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fdf/env.h>
#include <lib/fdf/testing.h>

#include <gtest/gtest.h>

namespace {

class HciTransportServer : public fidl::WireServer<fuchsia_hardware_bluetooth::HciTransport> {
 public:
  fuchsia_hardware_bluetooth::HciService::InstanceHandler GetInstanceHandler() {
    fuchsia_hardware_bluetooth::HciService::InstanceHandler handler;
    zx::result result = handler.add_hci_transport(bindings_.CreateHandler(
        this, fdf::Dispatcher::GetCurrent()->async_dispatcher(), fidl::kIgnoreBindingClosure));
    ZX_ASSERT(result.is_ok());
    return handler;
  }

  std::vector<std::vector<uint8_t>>& sent_command_packets() { return sent_command_packets_; }

 private:
  // WireServer<HciTransport> overrides:
  void Send(::fuchsia_hardware_bluetooth::wire::SentPacket* request,
            SendCompleter::Sync& completer) override {
    if (request->is_command()) {
      sent_command_packets_.emplace_back(request->command().begin(), request->command().end());
      completer.Reply();
    }
  }
  void AckReceive(AckReceiveCompleter::Sync& completer) override {}
  void ConfigureSco(::fuchsia_hardware_bluetooth::wire::HciTransportConfigureScoRequest* request,
                    ConfigureScoCompleter::Sync& completer) override {}
  void SetSnoop(::fuchsia_hardware_bluetooth::wire::HciTransportSetSnoopRequest* request,
                SetSnoopCompleter::Sync& completer) override {}
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_hardware_bluetooth::HciTransport> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {}

  std::vector<std::vector<uint8_t>> sent_command_packets_;

  fidl::ServerBindingGroup<fuchsia_hardware_bluetooth::HciTransport> bindings_;
};

}  // namespace

class PassthroughTest : public ::testing::Test {
 public:
  void SetUp() override {
    node_server_.emplace("root");
    zx::result start_args = node_server_->CreateStartArgsAndServe();
    ASSERT_EQ(ZX_OK, start_args.status_value());

    test_environment_.emplace();
    zx::result result =
        test_environment_->Initialize(std::move(start_args->incoming_directory_server));
    ASSERT_EQ(ZX_OK, result.status_value());

    auto hci_proto_handler = hci_server_.GetInstanceHandler();
    zx::result add_service_result =
        test_environment_->incoming_directory().AddService<fuchsia_hardware_bluetooth::HciService>(
            std::move(hci_proto_handler));
    ASSERT_EQ(ZX_OK, add_service_result.status_value());

    zx::result start_result =
        runtime_.RunToCompletion(driver_.Start(std::move(start_args->start_args)));
    ASSERT_EQ(ZX_OK, start_result.status_value());
  }

  void TearDown() override {
    zx::result prepare_stop_result = runtime_.RunToCompletion(driver_.PrepareStop());
    EXPECT_EQ(ZX_OK, prepare_stop_result.status_value());

    test_environment_.reset();
    node_server_.reset();

    runtime_.ShutdownAllDispatchers(fdf::Dispatcher::GetCurrent()->get());
  }

  fdf_testing::DriverUnderTest<bt::passthrough::PassthroughDevice>& driver() { return driver_; }

  std::optional<fdf_testing::TestNode>& node_server() { return node_server_; }

  async_dispatcher_t* dispatcher() { return fdf::Dispatcher::GetCurrent()->async_dispatcher(); }

  HciTransportServer& hci_server() { return hci_server_; }

 private:
  // Attaches a foreground dispatcher for us automatically.
  fdf_testing::DriverRuntime runtime_;

  HciTransportServer hci_server_;

  std::optional<fdf_testing::TestNode> node_server_;
  std::optional<fdf_testing::TestEnvironment> test_environment_;
  fdf_testing::DriverUnderTest<bt::passthrough::PassthroughDevice> driver_;
};

TEST_F(PassthroughTest, Lifecycle) {}

TEST_F(PassthroughTest, DevfsConnectAndSendCommand) {
  zx::result<zx::channel> connect_result =
      node_server()->children().at("bt-hci-passthrough").ConnectToDevice();
  ASSERT_TRUE(connect_result.is_ok());
  fidl::ClientEnd<fuchsia_hardware_bluetooth::Vendor> client_end(std::move(connect_result.value()));
  fidl::WireClient<fuchsia_hardware_bluetooth::Vendor> client(std::move(client_end), dispatcher());

  std::optional<fidl::ClientEnd<fuchsia_hardware_bluetooth::HciTransport>> hci_client_end;
  client->OpenHciTransport().Then(
      [&](fidl::WireUnownedResult<fuchsia_hardware_bluetooth::Vendor::OpenHciTransport>& result) {
        ASSERT_TRUE(result->is_ok());
        hci_client_end = std::move(result->value()->channel);
      });
  fdf_testing_run_until_idle();
  ASSERT_TRUE(hci_client_end);

  fidl::WireClient<fuchsia_hardware_bluetooth::HciTransport> hci_client(std::move(*hci_client_end),
                                                                        dispatcher());
  bool responded = false;
  uint8_t arr[1] = {3};
  fidl::VectorView vec_view = fidl::VectorView<uint8_t>::FromExternal(arr);
  fidl::ObjectView object_view =
      fidl::ObjectView<fidl::VectorView<uint8_t>>::FromExternal(&vec_view);
  hci_client->Send(::fuchsia_hardware_bluetooth::wire::SentPacket::WithCommand(object_view))
      .Then([&](fidl::WireUnownedResult<fuchsia_hardware_bluetooth::HciTransport::Send>& result) {
        responded = true;
      });
  fdf_testing_run_until_idle();
  ASSERT_TRUE(responded);

  ASSERT_EQ(hci_server().sent_command_packets().size(), 1u);
  EXPECT_EQ(hci_server().sent_command_packets()[0][0], arr[0]);

  auto endpoint = hci_client.UnbindMaybeGetEndpoint();
}
