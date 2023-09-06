// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_LD_TESTING_MOCK_LOADER_SERVICE_H_
#define LIB_LD_TESTING_MOCK_LOADER_SERVICE_H_

#include <fidl/fuchsia.ldsvc/cpp/wire.h>
#include <lib/zx/result.h>

#include <string_view>

#include <gmock/gmock.h>

namespace ld::testing {

class MockServer;

// MockLoaderService is a mock interface for testing that specific requests
// are made over the fuchsia.ldsvc.Loader protocol.
//
// This class initializes a mock server that serves the fuchsia.ldsvc.Loader
// protocol and provides a reference to a FIDL client to make requests with.
// `Expect*` handlers are provided for each protocol request so that the test
// caller may add an expectation that the method is called and define what the
// mock server should return in its response. For example:
//
// ```
// MockLoaderService mock_loader_service;
// ASSERT_NO_FATAL_FAILURE(mock_loader_service.Init());
// mock_loader_service.ExpectLoadObject("foo.so", zx::ok(zx::vmo()));
// ...
// ```
//
// The private `MockServer` is a StrictMock that will enforce that the test
// calls the `Expect*` handler for every request the mock server receives.
//
// If there are multiple Expect* handles set for the MockLoaderService, the
// test will verify the requests are made in the order of the Expect* calls.
class MockLoaderService {
 public:
  MockLoaderService();

  MockLoaderService(const MockLoaderService&) = delete;

  MockLoaderService(MockLoaderService&&) = delete;

  ~MockLoaderService();

  void Init();

  void ExpectLoadObject(std::string_view name, zx::result<zx::vmo> expected_result);

  void ExpectConfig(std::string_view name, zx::result<> expected_result);

  fidl::ClientEnd<fuchsia_ldsvc::Loader>& client() { return mock_client_; }

 private:
  std::unique_ptr<::testing::StrictMock<MockServer>> mock_server_;
  fidl::ClientEnd<fuchsia_ldsvc::Loader> mock_client_;
  // The sequence guard enforces the fuchsia.ldsvc.Loader requests are made in
  // the order that Expect* functions are called.
  ::testing::InSequence sequence_guard_;
};

}  // namespace ld::testing

#endif  // LIB_LD_TESTING_MOCK_LOADER_SERVICE_H_
