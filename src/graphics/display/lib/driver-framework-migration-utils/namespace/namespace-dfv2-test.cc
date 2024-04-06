// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/driver-framework-migration-utils/namespace/namespace-dfv2.h"

#include <fidl/fuchsia.component.runner/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/fidl.h>
#include <fidl/test.display.namespace/cpp/fidl.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>

#include <gtest/gtest.h>

#include "src/lib/testing/predicates/status.h"

namespace display {

namespace {

class MultiProtocolService : public fidl::Server<test_display_namespace::Incrementer>,
                             public fidl::Server<test_display_namespace::Decrementer> {
 public:
  MultiProtocolService() = default;
  ~MultiProtocolService() = default;

  // implements [`test.display.namespace/Incrementer`].
  void Increment(IncrementRequest& request, IncrementCompleter::Sync& completer) override {
    ZX_DEBUG_ASSERT(request.x() >= -32768);
    ZX_DEBUG_ASSERT(request.x() <= 32768);
    completer.Reply({{.result = request.x() + 1}});
  }

  // implements [`test.display.namespace/Decrementer`].
  void Decrement(DecrementRequest& request, DecrementCompleter::Sync& completer) override {
    ZX_DEBUG_ASSERT(request.x() >= -32768);
    ZX_DEBUG_ASSERT(request.x() <= 32768);
    completer.Reply({{.result = request.x() - 1}});
  }

  test_display_namespace::MultiProtocolService::InstanceHandler GetServiceInstanceHandler(
      async_dispatcher_t* dispatcher) {
    return test_display_namespace::MultiProtocolService::InstanceHandler({
        .incrementer =
            incrementer_bindings_.CreateHandler(this, dispatcher, fidl::kIgnoreBindingClosure),
        .decrementer =
            decrementer_bindings_.CreateHandler(this, dispatcher, fidl::kIgnoreBindingClosure),
    });
  }

 private:
  fidl::ServerBindingGroup<test_display_namespace::Incrementer> incrementer_bindings_;
  fidl::ServerBindingGroup<test_display_namespace::Decrementer> decrementer_bindings_;
};

class SingleProtocolService : public fidl::Server<test_display_namespace::Echo> {
 public:
  SingleProtocolService() = default;

  // implements [`test.display.namespace/Echo`].
  void EchoInt32(EchoInt32Request& request, EchoInt32Completer::Sync& completer) override {
    completer.Reply({{.result = request.x()}});
  }

  test_display_namespace::SingleProtocolService::InstanceHandler GetServiceInstanceHandler(
      async_dispatcher_t* dispatcher) {
    return test_display_namespace::SingleProtocolService::InstanceHandler({
        .echo = bindings_.CreateHandler(this, dispatcher, fidl::kIgnoreBindingClosure),
    });
  }

 private:
  fidl::ServerBindingGroup<test_display_namespace::Echo> bindings_;
};

class NamespaceDfv2Test : public testing::Test {
 public:
  // implements `testing::Test`.
  void SetUp() override {
    zx::result<fdf_testing::TestNode::CreateStartArgsResult> start_args_result =
        node_server_.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
    ASSERT_TRUE(start_args_result.is_ok());

    zx::result test_environment_init_result =
        test_environment_.SyncCall(&fdf_testing::TestEnvironment::Initialize,
                                   std::move(start_args_result->incoming_directory_server));
    ASSERT_TRUE(test_environment_init_result.is_ok());

    test_environment_.SyncCall([&](fdf_testing::TestEnvironment* environment) {
      zx::result<> multi_protocol_add_service_result =
          environment->incoming_directory()
              .AddService<test_display_namespace::MultiProtocolService>(
                  multi_protocol_service_.GetServiceInstanceHandler(
                      env_dispatcher_->async_dispatcher()),
                  "multi-protocol-fragment");
      ASSERT_OK(multi_protocol_add_service_result.status_value());

      zx::result<> single_protocol_add_service_result =
          environment->incoming_directory()
              .AddService<test_display_namespace::SingleProtocolService>(
                  single_protocol_service_.GetServiceInstanceHandler(
                      env_dispatcher_->async_dispatcher()),
                  "single-protocol-fragment");
      ASSERT_OK(single_protocol_add_service_result.status_value());
    });

    zx::result<fdf::Namespace> namespace_result =
        fdf::Namespace::Create(*start_args_result->start_args.incoming());
    ASSERT_OK(namespace_result.status_value());

    namespace_ = std::move(namespace_result).value();
  }

  // implements `testing::Test`.
  void TearDown() override {}

 protected:
  MultiProtocolService multi_protocol_service_;
  SingleProtocolService single_protocol_service_;

  fdf_testing::DriverRuntime runtime_;
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();

  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server_{
      env_dispatcher_->async_dispatcher(), std::in_place, std::string("root")};
  async_patterns::TestDispatcherBound<fdf_testing::TestEnvironment> test_environment_{
      env_dispatcher_->async_dispatcher(), std::in_place};

  fdf::Namespace namespace_;
};

TEST_F(NamespaceDfv2Test, ConnectToFidlProtocol) {
  zx::result<std::unique_ptr<Namespace>> namespace_result = NamespaceDfv2::Create(&namespace_);
  ASSERT_OK(namespace_result.status_value());
  std::unique_ptr<Namespace> incoming = std::move(namespace_result).value();

  zx::result<fidl::ClientEnd<test_display_namespace::Incrementer>> connect_result =
      incoming->Connect<test_display_namespace::MultiProtocolService::Incrementer>(
          "multi-protocol-fragment");
  ASSERT_OK(connect_result.status_value());

  fidl::SyncClient incrementer_client(std::move(connect_result).value());
  fidl::Result increment_result = incrementer_client->Increment({{.x = 1}});
  ASSERT_TRUE(increment_result.is_ok());
  EXPECT_EQ(increment_result.value().result(), 2);
}

TEST_F(NamespaceDfv2Test, ConnectToFidlProtocolUsingProvidedServerEnd) {
  zx::result<std::unique_ptr<Namespace>> namespace_result = NamespaceDfv2::Create(&namespace_);
  ASSERT_OK(namespace_result.status_value());
  std::unique_ptr<Namespace> incoming = std::move(namespace_result).value();

  zx::result<fidl::Endpoints<test_display_namespace::Incrementer>> incrementer_endpoints_result =
      fidl::CreateEndpoints<test_display_namespace::Incrementer>();
  ASSERT_OK(incrementer_endpoints_result.status_value());
  auto [incrementer_client_end, incrementer_server_end] =
      std::move(incrementer_endpoints_result).value();

  zx::result<> connect_result =
      incoming->Connect<test_display_namespace::MultiProtocolService::Incrementer>(
          std::move(incrementer_server_end), "multi-protocol-fragment");
  ASSERT_OK(connect_result.status_value());

  fidl::SyncClient incrementer_client(std::move(incrementer_client_end));
  fidl::Result increment_result = incrementer_client->Increment({{.x = 1}});
  ASSERT_TRUE(increment_result.is_ok());
  EXPECT_EQ(increment_result.value().result(), 2);
}

TEST_F(NamespaceDfv2Test, ConnectToDifferentServiceMemberProtocols) {
  zx::result<std::unique_ptr<Namespace>> namespace_result = NamespaceDfv2::Create(&namespace_);
  ASSERT_OK(namespace_result.status_value());
  std::unique_ptr<Namespace> incoming = std::move(namespace_result).value();

  zx::result<fidl::ClientEnd<test_display_namespace::Incrementer>> connect_incrementer_result =
      incoming->Connect<test_display_namespace::MultiProtocolService::Incrementer>(
          "multi-protocol-fragment");
  ASSERT_OK(connect_incrementer_result.status_value());

  zx::result<fidl::ClientEnd<test_display_namespace::Decrementer>> connect_decrementer_result =
      incoming->Connect<test_display_namespace::MultiProtocolService::Decrementer>(
          "multi-protocol-fragment");
  ASSERT_OK(connect_decrementer_result.status_value());

  fidl::SyncClient incrementer_client(std::move(connect_incrementer_result).value());
  fidl::Result increment_result = incrementer_client->Increment({{.x = 1}});
  ASSERT_TRUE(increment_result.is_ok());
  EXPECT_EQ(increment_result.value().result(), 2);

  fidl::SyncClient decrementer_client(std::move(connect_decrementer_result).value());
  fidl::Result decrement_result = decrementer_client->Decrement({{.x = 1}});
  ASSERT_TRUE(decrement_result.is_ok());
#include <lib/fdf/cpp/dispatcher.h>
  EXPECT_EQ(decrement_result.value().result(), 0);
}

TEST_F(NamespaceDfv2Test, ConnectToDifferentServiceMemberProtocolsUsingProvidedServerEnd) {
  zx::result<std::unique_ptr<Namespace>> namespace_result = NamespaceDfv2::Create(&namespace_);
  ASSERT_OK(namespace_result.status_value());
  std::unique_ptr<Namespace> incoming = std::move(namespace_result).value();

  zx::result<fidl::Endpoints<test_display_namespace::Incrementer>> incrementer_endpoints_result =
      fidl::CreateEndpoints<test_display_namespace::Incrementer>();
  ASSERT_OK(incrementer_endpoints_result.status_value());
  auto [incrementer_client_end, incrementer_server_end] =
      std::move(incrementer_endpoints_result).value();

  zx::result<fidl::Endpoints<test_display_namespace::Decrementer>> decrementer_endpoints_result =
      fidl::CreateEndpoints<test_display_namespace::Decrementer>();
  ASSERT_OK(decrementer_endpoints_result.status_value());
  auto [decrementer_client_end, decrementer_server_end] =
      std::move(decrementer_endpoints_result).value();

  zx::result<> connect_incrementer_result =
      incoming->Connect<test_display_namespace::MultiProtocolService::Incrementer>(
          std::move(incrementer_server_end), "multi-protocol-fragment");
  ASSERT_OK(connect_incrementer_result.status_value());

  zx::result<> connect_decrementer_result =
      incoming->Connect<test_display_namespace::MultiProtocolService::Decrementer>(
          std::move(decrementer_server_end), "multi-protocol-fragment");
  ASSERT_OK(connect_decrementer_result.status_value());

  fidl::SyncClient incrementer_client(std::move(incrementer_client_end));
  fidl::Result increment_result = incrementer_client->Increment({{.x = 1}});
  ASSERT_TRUE(increment_result.is_ok());
  EXPECT_EQ(increment_result.value().result(), 2);

  fidl::SyncClient decrementer_client(std::move(decrementer_client_end));
  fidl::Result decrement_result = decrementer_client->Decrement({{.x = 1}});
  ASSERT_TRUE(decrement_result.is_ok());
  EXPECT_EQ(decrement_result.value().result(), 0);
}

TEST_F(NamespaceDfv2Test, ConnectToDifferentFragments) {
  zx::result<std::unique_ptr<Namespace>> namespace_result = NamespaceDfv2::Create(&namespace_);
  ASSERT_OK(namespace_result.status_value());
  std::unique_ptr<Namespace> incoming = std::move(namespace_result).value();

  zx::result<fidl::ClientEnd<test_display_namespace::Incrementer>> connect_incrementer_result =
      incoming->Connect<test_display_namespace::MultiProtocolService::Incrementer>(
          "multi-protocol-fragment");
  ASSERT_OK(connect_incrementer_result.status_value());

  zx::result<fidl::ClientEnd<test_display_namespace::Echo>> connect_echo_result =
      incoming->Connect<test_display_namespace::SingleProtocolService::Echo>(
          "single-protocol-fragment");
  ASSERT_OK(connect_echo_result.status_value());

  fidl::SyncClient incrementer_client(std::move(connect_incrementer_result).value());
  fidl::Result increment_result = incrementer_client->Increment({{.x = 1}});
  ASSERT_TRUE(increment_result.is_ok());
  EXPECT_EQ(increment_result.value().result(), 2);

  fidl::SyncClient echo_client(std::move(connect_echo_result).value());
  fidl::Result echo_result = echo_client->EchoInt32({{.x = 1}});
  ASSERT_TRUE(echo_result.is_ok());
  EXPECT_EQ(echo_result.value().result(), 1);
}

TEST_F(NamespaceDfv2Test, ConnectToDifferentFragmentsUsingProvidedServerEnd) {
  zx::result<std::unique_ptr<Namespace>> namespace_result = NamespaceDfv2::Create(&namespace_);
  ASSERT_OK(namespace_result.status_value());
  std::unique_ptr<Namespace> incoming = std::move(namespace_result).value();

  zx::result<fidl::Endpoints<test_display_namespace::Incrementer>> incrementer_endpoints_result =
      fidl::CreateEndpoints<test_display_namespace::Incrementer>();
  ASSERT_OK(incrementer_endpoints_result.status_value());
  auto [incrementer_client_end, incrementer_server_end] =
      std::move(incrementer_endpoints_result).value();

  zx::result<fidl::Endpoints<test_display_namespace::Echo>> echo_endpoints_result =
      fidl::CreateEndpoints<test_display_namespace::Echo>();
  ASSERT_OK(echo_endpoints_result.status_value());
  auto [echo_client_end, echo_server_end] = std::move(echo_endpoints_result).value();

  zx::result<> connect_incrementer_result =
      incoming->Connect<test_display_namespace::MultiProtocolService::Incrementer>(
          std::move(incrementer_server_end), "multi-protocol-fragment");
  ASSERT_OK(connect_incrementer_result.status_value());

  zx::result<> connect_echo_result =
      incoming->Connect<test_display_namespace::SingleProtocolService::Echo>(
          std::move(echo_server_end), "single-protocol-fragment");
  ASSERT_OK(connect_echo_result.status_value());

  fidl::SyncClient incrementer_client(std::move(incrementer_client_end));
  fidl::Result increment_result = incrementer_client->Increment({{.x = 1}});
  ASSERT_TRUE(increment_result.is_ok());
  EXPECT_EQ(increment_result.value().result(), 2);

  fidl::SyncClient echo_client(std::move(echo_client_end));
  fidl::Result echo_result = echo_client->EchoInt32({{.x = 1}});
  ASSERT_TRUE(echo_result.is_ok());
  EXPECT_EQ(echo_result.value().result(), 1);
}

TEST_F(NamespaceDfv2Test, ErrorOnInvalidFragment) {
  zx::result<std::unique_ptr<Namespace>> namespace_result = NamespaceDfv2::Create(&namespace_);
  ASSERT_OK(namespace_result.status_value());
  std::unique_ptr<Namespace> incoming = std::move(namespace_result).value();

  zx::result<fidl::ClientEnd<test_display_namespace::Incrementer>> connect_incrementer_result =
      incoming->Connect<test_display_namespace::MultiProtocolService::Incrementer>(
          "invalid-fragment");

  // Connect() connects to the protocol asynchronously; ZX_OK doesn't guarantee
  // a successful connection to the protocol, but all FIDL calls will fail.
  ASSERT_OK(connect_incrementer_result.status_value());

  fidl::SyncClient incrementer_client(std::move(connect_incrementer_result).value());
  fidl::Result increment_result = incrementer_client->Increment({{.x = 1}});
  ASSERT_TRUE(increment_result.is_error());
  EXPECT_EQ(increment_result.error_value().reason(), fidl::Reason::kPeerClosedWhileReading);

  zx::result<fidl::Endpoints<test_display_namespace::Incrementer>> incrementer_endpoints_result =
      fidl::CreateEndpoints<test_display_namespace::Incrementer>();
  ASSERT_OK(incrementer_endpoints_result.status_value());
  auto [incrementer_client_end, incrementer_server_end] =
      std::move(incrementer_endpoints_result).value();

  zx::result<> connect_incrementer_using_server_end_result =
      incoming->Connect<test_display_namespace::MultiProtocolService::Incrementer>(
          std::move(incrementer_server_end), "invalid-fragment");

  // Connect() connects to the protocol asynchronously; ZX_OK doesn't guarantee
  // a successful connection to the protocol, but all FIDL calls will fail.
  ASSERT_OK(connect_incrementer_using_server_end_result.status_value());

  fidl::SyncClient incrementer_client2(std::move(incrementer_client_end));
  fidl::Result increment_result2 = incrementer_client->Increment({{.x = 1}});
  ASSERT_TRUE(increment_result2.is_error());
  EXPECT_EQ(increment_result2.error_value().reason(), fidl::Reason::kPeerClosedWhileReading);
}

TEST_F(NamespaceDfv2Test, ErrorOnServiceDoesntMatchFragment) {
  zx::result<std::unique_ptr<Namespace>> namespace_result = NamespaceDfv2::Create(&namespace_);
  ASSERT_OK(namespace_result.status_value());
  std::unique_ptr<Namespace> incoming = std::move(namespace_result).value();

  zx::result<fidl::ClientEnd<test_display_namespace::Incrementer>> connect_incrementer_result =
      incoming->Connect<test_display_namespace::MultiProtocolService::Incrementer>(
          "single-protocol-fragment");

  // Connect() connects to the protocol asynchronously; ZX_OK doesn't guarantee
  // a successful connection to the protocol, but all FIDL calls will fail.
  ASSERT_OK(connect_incrementer_result.status_value());

  fidl::SyncClient incrementer_client(std::move(connect_incrementer_result).value());
  fidl::Result increment_result = incrementer_client->Increment({{.x = 1}});
  ASSERT_TRUE(increment_result.is_error());
  EXPECT_EQ(increment_result.error_value().reason(), fidl::Reason::kPeerClosedWhileReading);

  zx::result<fidl::Endpoints<test_display_namespace::Incrementer>> incrementer_endpoints_result =
      fidl::CreateEndpoints<test_display_namespace::Incrementer>();
  ASSERT_OK(incrementer_endpoints_result.status_value());
  auto [incrementer_client_end, incrementer_server_end] =
      std::move(incrementer_endpoints_result).value();

  zx::result<> connect_incrementer_using_server_end_result =
      incoming->Connect<test_display_namespace::MultiProtocolService::Incrementer>(
          std::move(incrementer_server_end), "single-protocol-fragment");

  // Connect() connects to the protocol asynchronously; ZX_OK doesn't guarantee
  // a successful connection to the protocol, but all FIDL calls will fail.
  ASSERT_OK(connect_incrementer_using_server_end_result.status_value());

  fidl::SyncClient incrementer_client2(std::move(incrementer_client_end));
  fidl::Result increment_result2 = incrementer_client->Increment({{.x = 1}});
  ASSERT_TRUE(increment_result2.is_error());
  EXPECT_EQ(increment_result2.error_value().reason(), fidl::Reason::kPeerClosedWhileReading);
}

}  // namespace

}  // namespace display
