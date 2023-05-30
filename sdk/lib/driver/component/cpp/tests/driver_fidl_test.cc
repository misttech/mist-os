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
class TestIncomingAndOutgoingFidlsBase : public ::testing::Test {
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

    // Store the start_args for the subclasses to use to start the driver.
    start_args_ = std::move(start_args->start_args);
  }

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

  async_dispatcher_t* env_dispatcher() { return test_env_dispatcher_.dispatcher(); }

 protected:
  fuchsia_driver_framework::DriverStartArgs& start_args() { return start_args_; }

 private:
  // This starts up the initial managed thread. It must come before the dispatcher.
  fdf_testing::DriverRuntimeEnv managed_runtime_env_;

  // Env dispatcher. Managed by driver runtime threads.
  fdf::TestSynchronizedDispatcher test_env_dispatcher_{fdf::kDispatcherManaged};

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

  fidl::ClientEnd<fuchsia_io::Directory> driver_outgoing_;

  fuchsia_driver_framework::DriverStartArgs start_args_;
};

// Set the driver dispatcher to default so we can access |driver()| directly.
class TestIncomingAndOutgoingFidlsDefaultDriver : public TestIncomingAndOutgoingFidlsBase {
 public:
  // Sync clients into the driver have to be ran on a background thread because the test thread is
  // where the driver will handle the call.
  static void RunSyncClientTask(fit::closure task) {
    // Spawn a separate thread to run the client task using an async::Loop.
    async::Loop loop{&kAsyncLoopConfigNeverAttachToThread};
    loop.StartThread();
    zx::result result = fdf::RunOnDispatcherSync(loop.dispatcher(), std::move(task));
    ASSERT_EQ(ZX_OK, result.status_value());
  }

  void SetUp() override {
    TestIncomingAndOutgoingFidlsBase::SetUp();
    zx::result start_result = driver_.Start(std::move(start_args()));
    ASSERT_EQ(ZX_OK, start_result.status_value());
    ASSERT_EQ(ZX_OK, fdf::WaitFor(*start_result.value()).status_value());
  }

  void TearDown() override {
    std::shared_ptr<libsync::Completion> completion = driver_.PrepareStop();
    ASSERT_EQ(ZX_OK, fdf::WaitFor(*completion).status_value());
  }

  TestDriver* driver() { return *driver_; }
  async_dispatcher_t* driver_dispatcher() { return test_driver_dispatcher_.dispatcher(); }

 private:
  // Driver dispatcher set as the test's default dispatcher.
  fdf::TestSynchronizedDispatcher test_driver_dispatcher_{fdf::kDispatcherDefault};

  // The driver under test.
  fdf_testing::DriverUnderTest<TestDriver> driver_;
};

TEST_F(TestIncomingAndOutgoingFidlsDefaultDriver, ValidateDriverIncomingServices) {
  zx::result result = driver()->ValidateIncomingDriverService();
  ASSERT_EQ(ZX_OK, result.status_value());
  result = driver()->ValidateIncomingZirconService();
  ASSERT_EQ(ZX_OK, result.status_value());
}

TEST_F(TestIncomingAndOutgoingFidlsDefaultDriver, ConnectWithDevfs) {
  zx::result export_result = driver()->ExportDevfsNodeSync();
  ASSERT_EQ(ZX_OK, export_result.status_value());

  zx::result device_result = node_server().SyncCall([](fdf_testing::TestNode* root_node) {
    return root_node->children().at("devfs_node").ConnectToDevice();
  });

  ASSERT_EQ(ZX_OK, device_result.status_value());
  fidl::ClientEnd<fuchsia_driver_component_test::ZirconProtocol> device_client_end(
      std::move(device_result.value()));
  fidl::WireSyncClient<fuchsia_driver_component_test::ZirconProtocol> zircon_proto_client(
      std::move(device_client_end));

  RunSyncClientTask([zircon_proto_client = std::move(zircon_proto_client)]() {
    fidl::WireResult result = zircon_proto_client->ZirconMethod();
    ASSERT_EQ(ZX_OK, result.status());
    ASSERT_EQ(true, result.value().is_ok());
  });
}

TEST_F(TestIncomingAndOutgoingFidlsDefaultDriver, ConnectWithZirconService) {
  zx::result serve_result = driver()->ServeZirconService();
  ASSERT_EQ(ZX_OK, serve_result.status_value());

  zx::result result =
      component::ConnectAtMember<fuchsia_driver_component_test::ZirconService::Device>(
          CreateDriverSvcClient());
  ASSERT_EQ(ZX_OK, result.status_value());

  RunSyncClientTask([client_end = std::move(result.value())]() {
    fidl::WireResult wire_result = fidl::WireCall(client_end)->ZirconMethod();
    ASSERT_EQ(ZX_OK, wire_result.status());
    ASSERT_EQ(true, wire_result.value().is_ok());
  });
}

TEST_F(TestIncomingAndOutgoingFidlsDefaultDriver, ConnectWithDriverService) {
  zx::result serve_result = driver()->ServeDriverService();
  ASSERT_EQ(ZX_OK, serve_result.status_value());

  zx::result driver_connect_result =
      fdf::internal::DriverTransportConnect<fuchsia_driver_component_test::DriverService::Device>(
          CreateDriverSvcClient(), component::kDefaultInstance);
  ASSERT_EQ(ZX_OK, driver_connect_result.status_value());

  RunSyncClientTask([client_end = std::move(driver_connect_result.value())]() {
    fdf::Arena arena('TEST');
    fdf::WireUnownedResult wire_result = fdf::WireCall(client_end).buffer(arena)->DriverMethod();
    ASSERT_EQ(ZX_OK, wire_result.status());
    ASSERT_EQ(true, wire_result.value().is_ok());
  });
}

// Set the driver dispatcher to be managed so that we can make sync client calls into the driver
// hosted services directly from the test instead of using |RunSyncClientTask| from above.
class TestIncomingAndOutgoingFidlsManagedDriver : public TestIncomingAndOutgoingFidlsBase {
 public:
  void SetUp() override {
    TestIncomingAndOutgoingFidlsBase::SetUp();
    zx::result start_result =
        driver_.SyncCall(&fdf_testing::DriverUnderTest<TestDriver>::Start, std::move(start_args()));
    ASSERT_EQ(ZX_OK, start_result.status_value());
    ASSERT_EQ(ZX_OK, fdf::WaitFor(*start_result.value()).status_value());
  }

  void TearDown() override {
    std::shared_ptr<libsync::Completion> completion =
        driver_.SyncCall(&fdf_testing::DriverUnderTest<TestDriver>::PrepareStop);
    ASSERT_EQ(ZX_OK, fdf::WaitFor(*completion).status_value());
  }

  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<TestDriver>>& driver() {
    return driver_;
  }
  async_dispatcher_t* driver_dispatcher() { return test_driver_dispatcher_.dispatcher(); }

 private:
  // Driver dispatcher set as a managed dispatcher.
  fdf::TestSynchronizedDispatcher test_driver_dispatcher_{fdf::kDispatcherManaged};

  // The driver under test.
  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<TestDriver>> driver_{
      driver_dispatcher(), std::in_place};
};

TEST_F(TestIncomingAndOutgoingFidlsManagedDriver, ConnectWithDevfs) {
  driver().SyncCall([](fdf_testing::DriverUnderTest<TestDriver>* driver) {
    zx::result result = (*driver)->ExportDevfsNodeSync();
    ASSERT_EQ(ZX_OK, result.status_value());
  });

  zx::result device_result = node_server().SyncCall([](fdf_testing::TestNode* root_node) {
    return root_node->children().at("devfs_node").ConnectToDevice();
  });

  ASSERT_EQ(ZX_OK, device_result.status_value());
  fidl::ClientEnd<fuchsia_driver_component_test::ZirconProtocol> device_client_end(
      std::move(device_result.value()));
  fidl::WireSyncClient<fuchsia_driver_component_test::ZirconProtocol> zircon_proto_client(
      std::move(device_client_end));

  fidl::WireResult result = zircon_proto_client->ZirconMethod();
  ASSERT_EQ(ZX_OK, result.status());
  ASSERT_EQ(true, result.value().is_ok());
}

TEST_F(TestIncomingAndOutgoingFidlsManagedDriver, ConnectWithZirconService) {
  driver().SyncCall([](fdf_testing::DriverUnderTest<TestDriver>* driver) {
    zx::result result = (*driver)->ServeZirconService();
    ASSERT_EQ(ZX_OK, result.status_value());
  });

  zx::result result =
      component::ConnectAtMember<fuchsia_driver_component_test::ZirconService::Device>(
          CreateDriverSvcClient());
  ASSERT_EQ(ZX_OK, result.status_value());

  fidl::WireResult wire_result = fidl::WireCall(result.value())->ZirconMethod();
  ASSERT_EQ(ZX_OK, wire_result.status());
  ASSERT_EQ(true, wire_result.value().is_ok());
}

TEST_F(TestIncomingAndOutgoingFidlsManagedDriver, ConnectWithDriverService) {
  driver().SyncCall([](fdf_testing::DriverUnderTest<TestDriver>* driver) {
    zx::result result = (*driver)->ServeDriverService();
    ASSERT_EQ(ZX_OK, result.status_value());
  });

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
