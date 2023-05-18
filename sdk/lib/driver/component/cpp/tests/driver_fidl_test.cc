// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.component.test/cpp/driver/wire.h>
#include <fidl/fuchsia.driver.component.test/cpp/wire.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/driver/component/cpp/tests/test_driver.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime_env.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fdio/directory.h>

#include <gtest/gtest.h>

class ZirconProtocolServer
    : public fidl::WireServer<fuchsia_driver_component_test::ZirconProtocol> {
 public:
  fuchsia_driver_component_test::ZirconService::InstanceHandler GetInstanceHandler() {
    return fuchsia_driver_component_test::ZirconService::InstanceHandler({
        .device = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                          fidl::kIgnoreBindingClosure),
    });
  }

 private:
  // fidl::WireServer<fuchsia_driver_component_test::ZirconProtocol>
  void ZirconMethod(ZirconMethodCompleter::Sync& completer) override { completer.ReplySuccess(); }
  fidl::ServerBindingGroup<fuchsia_driver_component_test::ZirconProtocol> bindings_;
};

class DriverProtocolServer : public fdf::WireServer<fuchsia_driver_component_test::DriverProtocol> {
 public:
  fuchsia_driver_component_test::DriverService::InstanceHandler GetInstanceHandler() {
    return fuchsia_driver_component_test::DriverService::InstanceHandler({
        .device = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->get(),
                                          fidl::kIgnoreBindingClosure),
    });
  }

 private:
  // fdf::WireServer<fuchsia_driver_component_test::DriverProtocol>
  void DriverMethod(fdf::Arena& arena, DriverMethodCompleter::Sync& completer) override {
    fdf::Arena reply_arena('TEST');
    completer.buffer(reply_arena).ReplySuccess();
  }

  fdf::ServerBindingGroup<fuchsia_driver_component_test::DriverProtocol> bindings_;
};

// Sets up the environment to have both Zircon and Driver transport services.
class TestIncomingAndOutgoingFidls : public ::testing::Test {
 public:
  void SetUp() override {
    // Create start args
    zx::result start_args = node_server_.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
    ASSERT_EQ(ZX_OK, start_args.status_value());

    driver_outgoing_ = std::move(start_args->outgoing_directory_client);

    // Start the test environment
    zx::result init_result =
        test_environment_.SyncCall(&fdf_testing::TestEnvironment::Initialize,
                                   std::move(start_args->incoming_directory_server));
    ASSERT_EQ(ZX_OK, init_result.status_value());

    // Add the services to the environment.
    fuchsia_driver_component_test::ZirconService::InstanceHandler zircon_proto_handler =
        zircon_proto_server_.SyncCall(&ZirconProtocolServer::GetInstanceHandler);
    fuchsia_driver_component_test::DriverService::InstanceHandler driver_proto_handler =
        driver_proto_server_.SyncCall(&DriverProtocolServer::GetInstanceHandler);

    test_environment_.SyncCall([zircon_proto_handler = std::move(zircon_proto_handler),
                                driver_proto_handler = std::move(driver_proto_handler)](
                                   fdf_testing::TestEnvironment* env) mutable {
      zx::result result =
          env->incoming_directory().AddService<fuchsia_driver_component_test::ZirconService>(
              std::move(zircon_proto_handler));
      ASSERT_EQ(ZX_OK, result.status_value());

      result = env->incoming_directory().AddService<fuchsia_driver_component_test::DriverService>(
          std::move(driver_proto_handler));
      ASSERT_EQ(ZX_OK, result.status_value());
    });

    // Start the driver.
    zx::result driver = fdf_testing::StartDriver<TestDriver>(std::move(start_args->start_args),
                                                             test_driver_dispatcher_);
    ASSERT_EQ(ZX_OK, driver.status_value());
    driver_ = driver.value();
  }

  void TearDown() override {
    zx::result result = fdf_testing::TeardownDriver(driver_, test_driver_dispatcher_);
    ASSERT_EQ(ZX_OK, result.status_value());
  }

  // This MUST be accessed from the driver_dispatcher().
  // TODO(fxb/123068): Remove above comment.
  TestDriver* driver() { return driver_; }

  async_patterns::TestDispatcherBound<fdf_testing::TestNode>& node_server() { return node_server_; }

  fidl::ClientEnd<fuchsia_io::Directory> CreateDriverSvcClient() {
    // Open the svc directory in the driver's outgoing, and store a client to it.
    auto svc_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
    EXPECT_EQ(ZX_OK, svc_endpoints.status_value());

    zx_status_t status = fdio_open_at(driver_outgoing_.handle()->get(), "/svc",
                                      static_cast<uint32_t>(fuchsia_io::OpenFlags::kDirectory),
                                      svc_endpoints->server.TakeChannel().release());
    EXPECT_EQ(ZX_OK, status);
    return std::move(svc_endpoints->client);
  }

  async_dispatcher_t* driver_dispatcher() { return test_driver_dispatcher_.dispatcher(); }
  async_dispatcher_t* env_dispatcher() { return test_env_dispatcher_.dispatcher(); }

 private:
  // This starts up the initial managed thread. It must come before the dispatcher.
  fdf_testing::DriverRuntimeEnv managed_runtime_env_;

  // Driver dispatcher, managed by driver runtime threads because it has allow_sync_calls option.
  fdf::TestSynchronizedDispatcher test_driver_dispatcher_{fdf::kDispatcherNoDefaultAllowSync};

  // Env dispatcher. Managed by driver runtime threads because the test_driver_dispatcher has
  // allow_sync_calls option.
  fdf::TestSynchronizedDispatcher test_env_dispatcher_{{
      .is_default_dispatcher = false,
      .options = {},
      .dispatcher_name = "test-env-dispatcher",
  }};

  // Servers for the incoming FIDLs to the driver.
  async_patterns::TestDispatcherBound<ZirconProtocolServer> zircon_proto_server_{env_dispatcher(),
                                                                                 std::in_place};
  async_patterns::TestDispatcherBound<DriverProtocolServer> driver_proto_server_{env_dispatcher(),
                                                                                 std::in_place};

  // Serves the fdf::Node protocol to the driver.
  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server_{
      env_dispatcher(), std::in_place, std::string("root")};

  // The environment can serve both the Zircon and Driver transport based protocols to the driver.
  async_patterns::TestDispatcherBound<fdf_testing::TestEnvironment> test_environment_{
      env_dispatcher(), std::in_place};

  TestDriver* driver_;

  fidl::ClientEnd<fuchsia_io::Directory> driver_outgoing_;
};

TEST_F(TestIncomingAndOutgoingFidls, ValidateDriverIncomingServices) {
  // TODO(fxb/123068): Access directly from test thread.
  ASSERT_EQ(ZX_OK, fdf::RunOnDispatcherSync(driver_dispatcher(), [this]() {
                     zx::result result = driver()->ValidateIncomingDriverService();
                     ASSERT_EQ(ZX_OK, result.status_value());
                     result = driver()->ValidateIncomingZirconService();
                     ASSERT_EQ(ZX_OK, result.status_value());
                   }).status_value());
}

TEST_F(TestIncomingAndOutgoingFidls, ConnectWithZirconService) {
  // TODO(fxb/123068): Access directly from test thread.
  ASSERT_EQ(ZX_OK, fdf::RunOnDispatcherSync(driver_dispatcher(), [this]() {
                     zx::result result = driver()->ServeZirconService();
                     ASSERT_EQ(ZX_OK, result.status_value());
                   }).status_value());

  zx::result result =
      component::ConnectAtMember<fuchsia_driver_component_test::ZirconService::Device>(
          CreateDriverSvcClient());
  ASSERT_EQ(ZX_OK, result.status_value());

  fidl::WireResult wire_result = fidl::WireCall(result.value())->ZirconMethod();
  ASSERT_EQ(ZX_OK, wire_result.status());
  ASSERT_EQ(true, wire_result.value().is_ok());
}

TEST_F(TestIncomingAndOutgoingFidls, ConnectWithDriverService) {
  // TODO(fxb/123068): Access directly from test thread.
  ASSERT_EQ(ZX_OK, fdf::RunOnDispatcherSync(driver_dispatcher(), [this]() {
                     zx::result result = driver()->ServeDriverService();
                     ASSERT_EQ(ZX_OK, result.status_value());
                   }).status_value());

  zx::result driver_connect_result =
      fdf::internal::DriverTransportConnect<fuchsia_driver_component_test::DriverService::Device>(
          CreateDriverSvcClient(), component::kDefaultInstance);
  ASSERT_EQ(ZX_OK, driver_connect_result.status_value());

  fdf::Arena arena('TEST');
  fdf::WireUnownedResult wire_result =
      fdf::WireCall(driver_connect_result.value()).buffer(arena)->DriverMethod();
  ASSERT_EQ(ZX_OK, wire_result.status());
  ASSERT_EQ(true, wire_result.value().is_ok());
}
