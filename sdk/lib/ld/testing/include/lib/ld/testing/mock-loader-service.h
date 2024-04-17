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

// MockLoaderService is a mock interface for testing that specific requests
// are made over the fuchsia.ldsvc.Loader protocol.  It is controlled by the
// MockLoaderServiceForTest class (see below).
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

  // This must be called before other methods.
  // It should be used inside ASSERT_NO_FATAL_FAILURE(...).
  void Init();

  // Returns true if Init() has been called and succeeded.
  bool Ready() const { return static_cast<bool>(mock_server_); }

  // Tell the mock server to expect a LoadObject request for a VMO with `name`
  // and to return the `expected_result`.
  void ExpectLoadObject(std::string_view name, zx::result<zx::vmo> expected_result);

  // Tell the mock server to expect a Config request with `name` and to return
  // `expected_result`.
  void ExpectConfig(std::string_view name, zx::result<> expected_result);

  fidl::ClientEnd<fuchsia_ldsvc::Loader>& client() { return mock_client_; }

 private:
  class MockServer;

  std::unique_ptr<::testing::StrictMock<MockServer>> mock_server_;
  fidl::ClientEnd<fuchsia_ldsvc::Loader> mock_client_;
  // The sequence guard enforces the fuchsia.ldsvc.Loader requests are made in
  // the order that Expect* functions are called.
  ::testing::InSequence sequence_guard_;
};

// MockLoaderForTest is used by tests to manage an instance of the
// MockLoaderService. This class provides the public test API to prime the mock
// loader with FIDL request expectations and the responses the mock loader
// should return when receiving the request.
//
// MockLoaderForTest lazily initializes its mock_loader_ member, so tests must
// call Init() before interacting with the mock loader service:
//
// ```
// MockLoaderServiceForTest mock_;
// ASSERT_NO_FATAL_FAILURE(mock_.Init());
// mock_.ExpectLoadObject("foo.so", zx::ok(zx::vmo()));
// ...
// ```
class MockLoaderServiceForTest {
 public:
  // TODO(caslyn): in a subsequent CL this will get reworked so that the logic
  // to fetch a dependency and root module VMO will get baked into the
  // MockLoaderServiceForTest class and callers will not be passing anything
  // into the function call (the function call itself will describe what is
  // being fetched).
  template <typename GetVmo>
  void Needed(std::initializer_list<std::string_view> names, GetVmo&& get_vmo) {
    ASSERT_NO_FATAL_FAILURE(ReadyMock());
    for (std::string_view name : names) {
      // TODO(caslyn): the following three lines are repetitive in several
      // functions, they can be combined into a single callable function in
      // a future CL.
      zx::vmo vmo;
      ASSERT_NO_FATAL_FAILURE(vmo = get_vmo(name));
      mock_loader_->ExpectLoadObject(name, zx::ok(std::move(vmo)));
    }
  }

  template <typename GetVmo>
  void Needed(std::initializer_list<std::pair<std::string_view, bool>> name_found_pairs,
              GetVmo&& get_vmo) {
    ASSERT_NO_FATAL_FAILURE(ReadyMock());
    for (auto [name, found] : name_found_pairs) {
      if (found) {
        zx::vmo vmo;
        ASSERT_NO_FATAL_FAILURE(vmo = get_vmo(name));
        mock_loader_->ExpectLoadObject(name, zx::ok(std::move(vmo)));
      } else {
        mock_loader_->ExpectLoadObject(name, zx::error{ZX_ERR_NOT_FOUND});
      }
    }
  }

  void ExpectLoadObject(std::string_view name, zx::result<zx::vmo> expected_result) {
    ASSERT_NO_FATAL_FAILURE(ReadyMock());
    mock_loader_->ExpectLoadObject(name, std::move(expected_result));
  }

  template <typename GetVmo>
  void ExpectLoadObject(std::string_view name, GetVmo&& get_vmo) {
    ASSERT_NO_FATAL_FAILURE(ReadyMock());
    zx::vmo vmo;
    ASSERT_NO_FATAL_FAILURE(vmo = get_vmo(name));
    mock_loader_->ExpectLoadObject(name, zx::ok(std::move(vmo)));
  }

  void ExpectConfig(std::string_view config) {
    ASSERT_NO_FATAL_FAILURE(ReadyMock());
    mock_loader_->ExpectConfig(config, zx::ok());
  }

  zx::channel GetLdsvc() {
    zx::channel ldsvc;
    if (mock_loader_) {
      ldsvc = mock_loader_->client().TakeChannel();
    }
    return ldsvc;
  }

 private:
  void ReadyMock() {
    if (!mock_loader_) {
      mock_loader_ = std::make_unique<MockLoaderService>();
      ASSERT_NO_FATAL_FAILURE(mock_loader_->Init());
    }
  }

  std::unique_ptr<MockLoaderService> mock_loader_;
};

}  // namespace ld::testing

#endif  // LIB_LD_TESTING_MOCK_LOADER_SERVICE_H_
