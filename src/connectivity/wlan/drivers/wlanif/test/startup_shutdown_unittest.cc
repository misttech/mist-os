// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.wlan.fullmac/cpp/fidl.h>
#include <fidl/fuchsia.wlan.fullmac/cpp/test_base.h>
#include <fidl/fuchsia.wlan.sme/cpp/fidl.h>
#include <lib/driver/testing/cpp/driver_test.h>

#include <gtest/gtest.h>

#include "src/connectivity/wlan/drivers/wlanif/device.h"

namespace {

// Implements enough of the WlanFullmacImpl API to test that wlanif::Device can start up.
// The user can inject errors by inheriting from this class and overriding any relevant functions.
struct BaseWlanFullmacServerForStartup
    : public fidl::testing::TestBase<fuchsia_wlan_fullmac::WlanFullmacImpl> {
  explicit BaseWlanFullmacServerForStartup(
      fidl::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> server_end) {
    binding_.emplace(fdf_dispatcher_get_async_dispatcher(fdf_dispatcher_get_current_dispatcher()),
                     std::move(server_end), this, fidl::kIgnoreBindingClosure);
  }

  void Init(InitRequest& request, InitCompleter::Sync& completer) override {
    // Acquire/construct the WlanFullmacIfc, UsmeBootstrap, and GenericSme endpoints.
    fullmac_ifc_client_endpoint_ = std::move(request.ifc());
    auto usme_bootstrap_endpoints = fidl::CreateEndpoints<fuchsia_wlan_sme::UsmeBootstrap>();
    usme_bootstrap_client_ =
        fidl::Client(std::move(usme_bootstrap_endpoints.value().client),
                     fdf_dispatcher_get_async_dispatcher(fdf_dispatcher_get_current_dispatcher()));
    auto generic_sme_endpoints = fidl::CreateEndpoints<fuchsia_wlan_sme::GenericSme>();
    generic_sme_client_endpoint_ = std::move(generic_sme_endpoints.value().client);

    // Make the required UsmeBootstrap.Start during driver Init.
    usme_bootstrap_client_.value()
        ->Start(fuchsia_wlan_sme::UsmeBootstrapStartRequest(
            std::move(generic_sme_endpoints.value().server),
            fuchsia_wlan_sme::LegacyPrivacySupport(false, false)))
        .Then([&](fidl::Result<fuchsia_wlan_sme::UsmeBootstrap::Start>& result) mutable {
          ZX_ASSERT(result.is_ok());
          inspect_vmo_ = std::move(result->inspect_vmo());
        });

    fuchsia_wlan_fullmac::WlanFullmacImplInitResponse response;
    response.sme_channel() = usme_bootstrap_endpoints.value().server.TakeChannel();
    completer.Reply(zx::ok(std::move(response)));
  }

  void Query(QueryCompleter::Sync& completer) override {
    fuchsia_wlan_fullmac::WlanFullmacImplQueryResponse response;

    fuchsia_wlan_fullmac::BandCapability band_capability;
    band_capability.band({fuchsia_wlan_ieee80211::WlanBand::kTwoGhz})
        .basic_rates({{1}})
        .operating_channels({{1}});

    response.sta_addr({std::array<uint8_t, 6>{8, 8, 8, 8, 8, 8}})
        .role({fuchsia_wlan_common::WlanMacRole::kClient})
        .band_caps({{band_capability}});

    completer.Reply(fit::ok(std::move(response)));
  }

  void QueryMacSublayerSupport(QueryMacSublayerSupportCompleter::Sync& completer) override {
    fuchsia_wlan_common::MacSublayerSupport response(
        fuchsia_wlan_common::RateSelectionOffloadExtension(false),
        fuchsia_wlan_common::DataPlaneExtension(
            fuchsia_wlan_common::DataPlaneType::kGenericNetworkDevice),
        fuchsia_wlan_common::DeviceExtension(
            true, fuchsia_wlan_common::MacImplementationType::kFullmac, false));
    completer.Reply(fit::ok(response));
  }

  void QuerySecuritySupport(QuerySecuritySupportCompleter::Sync& completer) override {
    fuchsia_wlan_common::SecuritySupport response(fuchsia_wlan_common::SaeFeature(false, true),
                                                  fuchsia_wlan_common::MfpFeature(false));
    completer.Reply(fit::ok(response));
  }

  void QuerySpectrumManagementSupport(
      QuerySpectrumManagementSupportCompleter::Sync& completer) override {
    fuchsia_wlan_common::SpectrumManagementSupport response;
    completer.Reply(fit::ok(response));
  }

  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    ZX_PANIC("Not implemented: %s", name.c_str());
  }

  std::optional<fidl::ServerBinding<fuchsia_wlan_fullmac::WlanFullmacImpl>> binding_;
  std::optional<fidl::ClientEnd<::fuchsia_wlan_fullmac::WlanFullmacImplIfc>>
      fullmac_ifc_client_endpoint_;
  std::optional<fidl::Client<fuchsia_wlan_sme::UsmeBootstrap>> usme_bootstrap_client_;
  std::optional<fidl::ClientEnd<fuchsia_wlan_sme::GenericSme>> generic_sme_client_endpoint_;
  std::optional<zx::vmo> inspect_vmo_;
};

template <typename FullmacServer>
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

  std::optional<FullmacServer> wlan_fullmac_server_;
};

template <typename FullmacServer>
class TestConfig final {
 public:
  using DriverType = wlanif::Device;
  using EnvironmentType = WlanifDriverTestEnvironment<FullmacServer>;
};

template <typename FullmacServer>
using DriverTestType = fdf_testing::BackgroundDriverTest<TestConfig<FullmacServer>>;

TEST(StartupShutdownTest, BasicStartupShutdown) {
  DriverTestType<BaseWlanFullmacServerForStartup> driver_test;
  ASSERT_TRUE(driver_test.StartDriver().is_ok());
  ASSERT_TRUE(driver_test.StopDriver().is_ok());
}

TEST(StartupShutdownTest, StartFailsIfQueryFails) {
  struct QueryFails : public BaseWlanFullmacServerForStartup {
    using BaseWlanFullmacServerForStartup::BaseWlanFullmacServerForStartup;
    void Query(QueryCompleter::Sync& completer) override {
      completer.Reply(zx::error(ZX_ERR_INTERNAL));
    }
  };
  DriverTestType<QueryFails> driver_test;
  ASSERT_FALSE(driver_test.StartDriver().is_ok());
}

TEST(StartupShutdownTest, StartFailsIfQueryMacSublayerSupportFails) {
  struct QueryMacSublayerSupportFails : public BaseWlanFullmacServerForStartup {
    using BaseWlanFullmacServerForStartup::BaseWlanFullmacServerForStartup;
    void QueryMacSublayerSupport(QueryMacSublayerSupportCompleter::Sync& completer) override {
      completer.Reply(zx::error(ZX_ERR_INTERNAL));
    }
  };
  DriverTestType<QueryMacSublayerSupportFails> driver_test;
  ASSERT_FALSE(driver_test.StartDriver().is_ok());
}

TEST(StartupShutdownTest, StartFailsIfQuerySecuritySupport) {
  struct QuerySecuritySupportFails : public BaseWlanFullmacServerForStartup {
    using BaseWlanFullmacServerForStartup::BaseWlanFullmacServerForStartup;
    void QuerySecuritySupport(QuerySecuritySupportCompleter::Sync& completer) override {
      completer.Reply(zx::error(ZX_ERR_INTERNAL));
    }
  };
  DriverTestType<QuerySecuritySupportFails> driver_test;
  ASSERT_FALSE(driver_test.StartDriver().is_ok());
}

TEST(StartupShutdownTest, StartFailsIfQuerySpectrumManagementSupport) {
  struct QuerySpectrumManagementSupportFails : public BaseWlanFullmacServerForStartup {
    using BaseWlanFullmacServerForStartup::BaseWlanFullmacServerForStartup;
    void QuerySpectrumManagementSupport(
        QuerySpectrumManagementSupportCompleter::Sync& completer) override {
      completer.Reply(zx::error(ZX_ERR_INTERNAL));
    }
  };
  DriverTestType<QuerySpectrumManagementSupportFails> driver_test;
  ASSERT_FALSE(driver_test.StartDriver().is_ok());
}

TEST(StartupShutdownTest, DroppingIfcChannelCausesDroppedNode) {
  DriverTestType<BaseWlanFullmacServerForStartup> driver_test;
  ASSERT_TRUE(driver_test.StartDriver().is_ok());

  driver_test.RunInEnvironmentTypeContext([](auto& env) {
    BaseWlanFullmacServerForStartup& server = env.wlan_fullmac_server_.value();
    server.fullmac_ifc_client_endpoint_.reset();
  });

  while (driver_test.RunInNodeContext<bool>([](auto& node) { return node.HasNode(); })) {
    std::this_thread::sleep_for(std::chrono::milliseconds(5));
  }

  // Note: wlanif::Device completes PrepareStop with errors, but it doesn't get propagated by the
  // framework.
  ASSERT_TRUE(driver_test.StopDriver().is_ok());
}

TEST(StartupShutdownTest, DroppingGenericSmeChannelCausesDroppedNode) {
  DriverTestType<BaseWlanFullmacServerForStartup> driver_test;
  ASSERT_TRUE(driver_test.StartDriver().is_ok());

  driver_test.RunInEnvironmentTypeContext([](auto& env) {
    BaseWlanFullmacServerForStartup& server = env.wlan_fullmac_server_.value();
    server.generic_sme_client_endpoint_.reset();
  });

  while (driver_test.RunInNodeContext<bool>([](auto& node) { return node.HasNode(); })) {
    std::this_thread::sleep_for(std::chrono::milliseconds(5));
  }

  // Note: wlanif::Device completes PrepareStop with errors, but it doesn't get propagated by the
  // framework.
  ASSERT_TRUE(driver_test.StopDriver().is_ok());
}

}  // namespace
