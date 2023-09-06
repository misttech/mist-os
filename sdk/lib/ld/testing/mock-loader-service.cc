// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/ld/testing/mock-loader-service.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/dispatcher.h>

#include <gmock/gmock.h>

using ::testing::Return;

namespace ld::testing {

// A mock server implementation serving the fuchsia.ldsvc.Loader protocol. When
// it receives a request, it will invoke the associated MOCK_METHOD. If the
// test caller did not call Expect* for the request before it is made, then
// the MockServer will fail the test.
class MockServer : public fidl::WireServer<fuchsia_ldsvc::Loader> {
 public:
  MockServer() = default;

  ~MockServer() {
    if (backing_loop_) {
      backing_loop_->Shutdown();
    }
  }

  void Init(fidl::ServerEnd<fuchsia_ldsvc::Loader> server) {
    backing_loop_.emplace(&kAsyncLoopConfigNoAttachToCurrentThread);
    zx_status_t status = backing_loop_->StartThread("MockLoaderService");
    ASSERT_EQ(status, ZX_OK) << zx_status_get_string(status);
    fidl::BindServer(backing_loop_->dispatcher(), std::move(server), this);
  }

  MOCK_METHOD(zx::result<zx::vmo>, MockLoadObject, (std::string_view));

  MOCK_METHOD(zx::result<>, MockConfig, (std::string_view));

 private:
  // The fidl::WireServer<fuchsia_ldsvc::Loader> implementation, each FIDL
  // method will call into its associated gMock method when invoked.

  // Done is not used in ld tests.
  void Done(DoneCompleter::Sync& completer) override { ADD_FAILURE() << "unexpected Done call"; }

  void LoadObject(LoadObjectRequestView request, LoadObjectCompleter::Sync& completer) override {
    auto result = MockLoadObject(request->object_name.get());
    completer.Reply(result.status_value(), std::move(result).value_or(zx::vmo()));
  }

  void Config(ConfigRequestView request, ConfigCompleter::Sync& completer) override {
    auto result = MockConfig(request->config.get());
    completer.Reply(result.status_value());
  }

  // Clone is not used in ld tests.
  void Clone(CloneRequestView request, CloneCompleter::Sync& completer) override {
    ADD_FAILURE() << "unexpected Clone call";
  }

  std::optional<async::Loop> backing_loop_;
};

MockLoaderService::MockLoaderService() = default;

MockLoaderService::~MockLoaderService() = default;

void MockLoaderService::Init() {
  mock_server_ = std::make_unique<::testing::StrictMock<MockServer>>();
  auto endpoints = fidl::CreateEndpoints<fuchsia_ldsvc::Loader>();
  ASSERT_EQ(endpoints.status_value(), ZX_OK);
  ASSERT_NO_FATAL_FAILURE(mock_server_->Init(std::move(endpoints->server)));
  mock_client_ = std::move(endpoints->client);
}

void MockLoaderService::ExpectLoadObject(std::string_view name,
                                         zx::result<zx::vmo> expected_result) {
  EXPECT_CALL(*mock_server_, MockLoadObject(name)).WillOnce(Return(std::move(expected_result)));
}

void MockLoaderService::ExpectConfig(std::string_view name, zx::result<> expected_result) {
  EXPECT_CALL(*mock_server_, MockConfig(name)).WillOnce(Return(expected_result));
}

}  // namespace ld::testing
