// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_host/loader.h"

#include <fidl/fuchsia.ldsvc/cpp/wire_test_base.h>
#include <lib/async-loop/cpp/loop.h>

#include <gtest/gtest.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

namespace fldsvc = fuchsia_ldsvc;

namespace {

zx_koid_t GetKoid(zx::vmo& vmo) {
  zx_info_handle_basic_t info{};
  zx_status_t status = vmo.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  EXPECT_EQ(ZX_OK, status);
  return info.koid;
}

class TestEventHandler : public fidl::WireAsyncEventHandler<fldsvc::Loader> {
 public:
  fidl::Reason Reason() const { return error_.reason(); }

 private:
  void on_fidl_error(fidl::UnbindInfo error) { error_ = error; }

  fidl::UnbindInfo error_;
};

class TestLoader : public fidl::testing::WireTestBase<fldsvc::Loader> {
 public:
  TestLoader() : vmo_() {}
  explicit TestLoader(zx::vmo vmo) : vmo_(std::move(vmo)) {}

 private:
  // fidl::WireServer<fuchsia_ldsvc::Loader>
  void LoadObject(LoadObjectRequestView request, LoadObjectCompleter::Sync& completer) override {
    completer.Reply(ZX_OK, std::move(vmo_));
  }

  void Config(ConfigRequestView request, ConfigCompleter::Sync& completer) override {
    completer.Reply(ZX_OK);
  }

  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) override {
    printf("Not implemented: Loader::%s\n", name.data());
  }

  zx::vmo vmo_;
};

}  // namespace

class LoaderTest : public gtest::TestLoopFixture {};

TEST_F(LoaderTest, LoadObject) {
  auto endpoints = fidl::CreateEndpoints<fldsvc::Loader>();

  // Create VMO for backing loader reply.
  zx::vmo mylib_vmo;
  zx_status_t status = zx::vmo::create(zx_system_get_page_size(), 0, &mylib_vmo);
  ASSERT_EQ(ZX_OK, status);
  zx_koid_t mylib_koid = GetKoid(mylib_vmo);

  // Create backing loader.
  TestLoader backing_loader{std::move(mylib_vmo)};
  async::Loop backing_loop(&kAsyncLoopConfigNeverAttachToThread);
  fidl::BindServer(backing_loop.dispatcher(), std::move(endpoints->server), &backing_loader);
  ASSERT_EQ(ZX_OK, backing_loop.StartThread("loader-loop"));

  fidl::ClientEnd<fuchsia_io::File> file;
  driver_host::Loader::OverrideMap overrides;
  overrides.emplace("test_driver.so", std::move(file));
  // Create loader.
  fidl::ClientEnd loader_client = std::move(endpoints->client);
  driver_host::Loader loader(loader_client.borrow(), std::move(overrides));

  // Create loader client.
  endpoints = fidl::CreateEndpoints<fldsvc::Loader>();
  fidl::BindServer(dispatcher(), std::move(endpoints->server), &loader);
  fidl::WireClient<fldsvc::Loader> client(std::move(endpoints->client), dispatcher());

  // Test that loading a random library fetches a VMO from the backing loader.
  client->LoadObject("mylib.so").Then([mylib_koid](auto& result) {
    ASSERT_EQ(ZX_OK, result.status());
    auto* response = result.Unwrap();
    EXPECT_EQ(ZX_OK, response->rv);
    zx_koid_t actual_koid = GetKoid(response->object);
    EXPECT_EQ(mylib_koid, actual_koid);
  });

  ASSERT_TRUE(RunLoopUntilIdle());

  // Test that loading the driver library fetches a VMO from the loader.
  client->LoadObject("test_driver.so").Then([](auto& result) {
    ASSERT_EQ(ZX_OK, result.status());
    auto* response = result.Unwrap();
    EXPECT_EQ(ZX_ERR_BAD_HANDLE, response->rv);
  });

  ASSERT_TRUE(RunLoopUntilIdle());
}

TEST_F(LoaderTest, DoneClosesConnection) {
  auto endpoints = fidl::CreateEndpoints<fldsvc::Loader>();

  // Create backing loader.
  TestLoader backing_loader;
  async::Loop backing_loop(&kAsyncLoopConfigNeverAttachToThread);
  fidl::BindServer(backing_loop.dispatcher(), std::move(endpoints->server), &backing_loader);
  ASSERT_EQ(ZX_OK, backing_loop.StartThread("loader-loop"));

  // Create loader.
  driver_host::Loader loader(endpoints->client.borrow(), {});

  // Create event handler.
  TestEventHandler handler;

  // Create loader client.
  endpoints = fidl::CreateEndpoints<fldsvc::Loader>();
  fidl::BindServer(dispatcher(), std::move(endpoints->server), &loader);
  fidl::WireClient<fldsvc::Loader> client(std::move(endpoints->client), dispatcher(), &handler);

  // Test that done closes the connection.
  [[maybe_unused]] auto result = client->Done();

  ASSERT_TRUE(RunLoopUntilIdle());

  EXPECT_EQ(fidl::Reason::kPeerClosedWhileReading, handler.Reason());
}

TEST_F(LoaderTest, ConfigSucceeds) {
  auto endpoints = fidl::CreateEndpoints<fldsvc::Loader>();

  // Create backing loader.
  TestLoader backing_loader;
  async::Loop backing_loop(&kAsyncLoopConfigNeverAttachToThread);
  fidl::BindServer(backing_loop.dispatcher(), std::move(endpoints->server), &backing_loader);
  ASSERT_EQ(ZX_OK, backing_loop.StartThread("loader-loop"));

  // Create loader.
  fidl::ClientEnd loader_client = std::move(endpoints->client);
  driver_host::Loader loader(loader_client.borrow(), {});

  // Create loader client.
  endpoints = fidl::CreateEndpoints<fldsvc::Loader>();
  fidl::BindServer(dispatcher(), std::move(endpoints->server), &loader);
  fidl::WireClient<fldsvc::Loader> client(std::move(endpoints->client), dispatcher());

  // Test that config returns success.
  client->Config("").Then([](auto& result) {
    ASSERT_EQ(ZX_OK, result.status());
    auto* response = result.Unwrap();
    EXPECT_EQ(ZX_OK, response->rv);
  });

  ASSERT_TRUE(RunLoopUntilIdle());
}

TEST_F(LoaderTest, CloneSucceeds) {
  auto endpoints = fidl::CreateEndpoints<fldsvc::Loader>();

  // Create backing loader.
  TestLoader backing_loader;
  async::Loop backing_loop(&kAsyncLoopConfigNeverAttachToThread);
  fidl::BindServer(backing_loop.dispatcher(), std::move(endpoints->server), &backing_loader);
  ASSERT_EQ(ZX_OK, backing_loop.StartThread("loader-loop"));

  // Create loader.
  driver_host::Loader loader(endpoints->client.borrow(), {});

  // Create loader client.
  endpoints = fidl::CreateEndpoints<fldsvc::Loader>();
  fidl::BindServer(dispatcher(), std::move(endpoints->server), &loader);
  fidl::WireClient<fldsvc::Loader> client(std::move(endpoints->client), dispatcher());

  // Test that clone returns success.
  endpoints = fidl::CreateEndpoints<fldsvc::Loader>();
  client->Clone(std::move(endpoints->server)).Then([](auto& result) {
    ASSERT_EQ(ZX_OK, result.status());
    auto* response = result.Unwrap();
    EXPECT_EQ(ZX_OK, response->rv);
  });

  ASSERT_TRUE(RunLoopUntilIdle());
}

TEST_F(LoaderTest, NoBackingLoader) {
  auto endpoints = fidl::CreateEndpoints<fldsvc::Loader>();

  // Create loader.
  driver_host::Loader loader(endpoints->client.borrow(), {});

  // Close the server end of the backing loader channel.
  endpoints->server.reset();

  // Create loader client.
  endpoints = fidl::CreateEndpoints<fldsvc::Loader>();
  fidl::BindServer(dispatcher(), std::move(endpoints->server), &loader);
  fidl::WireClient<fldsvc::Loader> client(std::move(endpoints->client), dispatcher());

  // Test that functions that call the backing loader fail.
  client->LoadObject("mylib.so").Then([](auto& result) {
    ASSERT_EQ(ZX_OK, result.status());
    auto* response = result.Unwrap();
    EXPECT_EQ(ZX_ERR_BAD_HANDLE, response->rv);
  });
  client->Config("").Then([](auto& result) {
    ASSERT_EQ(ZX_OK, result.status());
    auto* response = result.Unwrap();
    EXPECT_EQ(ZX_ERR_BAD_HANDLE, response->rv);
  });

  ASSERT_TRUE(RunLoopUntilIdle());
}
