// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.wlan.fullmac/cpp/fidl.h>
#include <fidl/fuchsia.wlan.fullmac/cpp/test_base.h>
#include <lib/driver/testing/cpp/driver_test.h>

#include <gtest/gtest.h>

#include "src/connectivity/wlan/drivers/wlanif/device.h"
#include "src/connectivity/wlan/lib/mlme/fullmac/c-binding/testing/bindings_stubs.h"

struct FakeWlanFullmacServer final
    : public fidl::testing::TestBase<fuchsia_wlan_fullmac::WlanFullmacImpl> {
  explicit FakeWlanFullmacServer(fidl::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> server_end)
      : binding_(fdf_dispatcher_get_async_dispatcher(fdf_dispatcher_get_current_dispatcher()),
                 std::move(server_end), this, fidl::kIgnoreBindingClosure) {}

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {}
  fidl::ServerBinding<fuchsia_wlan_fullmac::WlanFullmacImpl> binding_;
};

struct WlanifDriverTestEnvironment : public fdf_testing::Environment {
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) final {
    fuchsia_wlan_fullmac::Service::InstanceHandler handler;
    ZX_ASSERT(handler
                  .add_wlan_fullmac_impl(
                      [this](fidl::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> server_end) {
                        wlan_fullmac_server_.emplace(std::move(server_end));
                      })
                  .is_ok());

    auto add_service_result =
        to_driver_vfs.AddService<fuchsia_wlan_fullmac::Service>(std::move(handler));
    ZX_ASSERT(add_service_result.is_ok());

    return zx::ok();
  }

  std::optional<FakeWlanFullmacServer> wlan_fullmac_server_;
};

class TestConfig final {
 public:
  using DriverType = wlanif::Device;
  using EnvironmentType = WlanifDriverTestEnvironment;
};

struct StartupShutdownTest : public ::testing::Test {
  fdf_testing::ForegroundDriverTest<TestConfig> driver_test_;
  bindings_stubs::FullmacMlmeBindingsStubs bindings_stubs_;
};

TEST_F(StartupShutdownTest, BasicStartupAndShutdownCallsFfi) {
  wlan_fullmac_mlme_handle_t fake_handle;
  libsync::Completion start_called;
  bindings_stubs_.start_fullmac_mlme_stub = [&](zx_handle_t) {
    start_called.Signal();
    return &fake_handle;
  };
  libsync::Completion stop_called;
  bindings_stubs_.stop_fullmac_mlme_stub = [&](wlan_fullmac_mlme_handle_t* handle) {
    ASSERT_EQ(handle, &fake_handle);
    stop_called.Signal();
  };

  libsync::Completion delete_called;
  bindings_stubs_.delete_fullmac_mlme_stub = [&](wlan_fullmac_mlme_handle_t* handle) {
    ASSERT_EQ(handle, &fake_handle);
    delete_called.Signal();
  };

  ASSERT_TRUE(driver_test_.StartDriver().is_ok());
  ASSERT_TRUE(start_called.signaled());
  ASSERT_FALSE(stop_called.signaled());
  ASSERT_FALSE(delete_called.signaled());

  ASSERT_TRUE(driver_test_.StopDriver().is_ok());
  ASSERT_TRUE(stop_called.signaled());
  ASSERT_FALSE(delete_called.signaled());

  driver_test_.ShutdownAndDestroyDriver();
  ASSERT_TRUE(delete_called.signaled());
}

TEST_F(StartupShutdownTest, StartFailsIfMlmeStartFails) {
  bindings_stubs_.start_fullmac_mlme_stub = [](zx_handle_t) { return nullptr; };
  ZX_ASSERT(driver_test_.StartDriver().is_error());
}
