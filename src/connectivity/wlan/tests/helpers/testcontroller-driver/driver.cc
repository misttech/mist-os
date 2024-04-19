// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.wlan.fullmac/cpp/driver/fidl.h>
#include <fidl/test.wlan.testcontroller/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/logging/cpp/structured_logger.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/stdcompat/source_location.h>

#include <sstream>
#include <vector>

#include <wlan/drivers/log.h>

namespace wlan_testcontroller {
namespace {

// Retrieve the underlying zx_status_t from a FIDL error, which may be either a domain or framework
// error. This assumes that the type of the domain_error is a zx_status_t, not a domain-specific
// error enum.
template <typename ErrorType>
zx_status_t FidlErrorToStatus(const ErrorType& e) {
  return e.is_domain_error() ? e.domain_error() : e.framework_error().status();
}

// Returns a lambda that can be used as the callback passed to |Then| or |ThenExactlyOnce| on the
// return value from making an async FIDL request over |bridge_client_|.
// The returned lambda will forward the result to the provided |completer|.
template <typename FidlMethod, typename AsyncCompleter>
auto ForwardResult(AsyncCompleter completer,
                   cpp20::source_location loc = cpp20::source_location::current()) {
  // This function can be applied to FIDL protocols on both the channel and driver transports.
  constexpr bool channel_transport =
      std::is_same_v<typename FidlMethod::Protocol::Transport, fidl::internal::ChannelTransport>;
  using ResultType = typename std::conditional_t<channel_transport, fidl::Result<FidlMethod>,
                                                 fdf::Result<FidlMethod>>;

  return [completer = std::move(completer), loc](ResultType& result) mutable {
    FDF_LOG(TRACE, "Forwarding result for %s", loc.function_name());
    if (result.is_error()) {
      FDF_LOG(ERROR, "Result not ok for %s: %s", loc.function_name(),
              result.error_value().FormatDescription().c_str());
    }

    constexpr bool has_reply = FidlMethod::kHasNonEmptyUserFacingResponse ||
                               FidlMethod::kHasDomainError || FidlMethod::kHasFrameworkError;
    if constexpr (has_reply) {
      completer.Reply(result.map_error([](auto error) { return FidlErrorToStatus(error); }));
    } else {
      completer.Reply();
    }
  };
}

}  // namespace

// Server that forwards calls from WlanFullmacImplIfcChannel to WlanFullmacImplIfc.
class WlanFullmacImplIfcToDriverBridge
    : public fidl::Server<fuchsia_wlan_fullmac::WlanFullmacImplIfcBridge> {
  using WlanFullmacImplIfcBridge = fuchsia_wlan_fullmac::WlanFullmacImplIfcBridge;
  using WlanFullmacImplIfc = fuchsia_wlan_fullmac::WlanFullmacImplIfc;

 public:
  explicit WlanFullmacImplIfcToDriverBridge(
      fdf_dispatcher_t* driver_dispatcher,
      fidl::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImplIfcBridge> server_end,
      fdf::ClientEnd<fuchsia_wlan_fullmac::WlanFullmacImplIfc> bridge_client_end)
      : binding_(fdf_dispatcher_get_async_dispatcher(driver_dispatcher), std::move(server_end),
                 this, fidl::kIgnoreBindingClosure),
        bridge_client_(std::move(bridge_client_end), driver_dispatcher) {
    WLAN_TRACE_DURATION();
  }

  void OnScanResult(OnScanResultRequest& request, OnScanResultCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->OnScanResult(request).ThenExactlyOnce(
        ForwardResult<WlanFullmacImplIfc::OnScanResult>(completer.ToAsync()));
  }
  void OnScanEnd(OnScanEndRequest& request, OnScanEndCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->OnScanEnd(request).ThenExactlyOnce(
        ForwardResult<WlanFullmacImplIfc::OnScanEnd>(completer.ToAsync()));
  }
  void ConnectConf(ConnectConfRequest& request, ConnectConfCompleter::Sync& completer) override {}
  void RoamConf(RoamConfRequest& request, RoamConfCompleter::Sync& completer) override {}
  void AuthInd(AuthIndRequest& request, AuthIndCompleter::Sync& completer) override {}
  void DeauthConf(DeauthConfRequest& request, DeauthConfCompleter::Sync& completer) override {}
  void DeauthInd(DeauthIndRequest& request, DeauthIndCompleter::Sync& completer) override {}
  void AssocInd(AssocIndRequest& request, AssocIndCompleter::Sync& completer) override {}
  void DisassocConf(DisassocConfRequest& request, DisassocConfCompleter::Sync& completer) override {
  }
  void DisassocInd(DisassocIndRequest& request, DisassocIndCompleter::Sync& completer) override {}
  void StartConf(StartConfRequest& request, StartConfCompleter::Sync& completer) override {}
  void StopConf(StopConfRequest& request, StopConfCompleter::Sync& completer) override {}
  void EapolConf(EapolConfRequest& request, EapolConfCompleter::Sync& completer) override {}
  void OnChannelSwitch(OnChannelSwitchRequest& request,
                       OnChannelSwitchCompleter::Sync& completer) override {}
  void SignalReport(SignalReportRequest& request, SignalReportCompleter::Sync& completer) override {
  }
  void EapolInd(EapolIndRequest& request, EapolIndCompleter::Sync& completer) override {}
  void OnPmkAvailable(OnPmkAvailableRequest& request,
                      OnPmkAvailableCompleter::Sync& completer) override {}
  void SaeHandshakeInd(SaeHandshakeIndRequest& request,
                       SaeHandshakeIndCompleter::Sync& completer) override {}
  void SaeFrameRx(SaeFrameRxRequest& request, SaeFrameRxCompleter::Sync& completer) override {}
  void OnWmmStatusResp(OnWmmStatusRespRequest& request,
                       OnWmmStatusRespCompleter::Sync& completer) override {}

 private:
  fidl::ServerBinding<fuchsia_wlan_fullmac::WlanFullmacImplIfcBridge> binding_;
  fdf::Client<fuchsia_wlan_fullmac::WlanFullmacImplIfc> bridge_client_;
};

// Server that forwards calls from WlanFullmacImpl to WlanFullmacImplBridge.
class WlanFullmacImplToChannelBridge : public fdf::Server<fuchsia_wlan_fullmac::WlanFullmacImpl> {
 private:
  using WlanFullmacImplBridge = fuchsia_wlan_fullmac::WlanFullmacImplBridge;

 public:
  explicit WlanFullmacImplToChannelBridge(
      fdf_dispatcher_t* driver_dispatcher,
      fdf::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> server_end,
      fidl::ClientEnd<fuchsia_wlan_fullmac::WlanFullmacImplBridge> bridge_client_end)
      : binding_(driver_dispatcher, std::move(server_end), this, fidl::kIgnoreBindingClosure),
        bridge_client_(std::move(bridge_client_end),
                       fdf_dispatcher_get_async_dispatcher(driver_dispatcher)),
        driver_dispatcher_(driver_dispatcher) {
    WLAN_TRACE_DURATION();
  }

  void Start(StartRequest& request, StartCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    // Start has to swap out the fdf::ClientEnd and instead pass along a fidl::ClientEnd to the
    // bridge server. This is necessary because the test case runs in a non-driver component, which
    // cannot use the fdf::ClientEnd.
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_wlan_fullmac::WlanFullmacImplIfcBridge>();
    if (endpoints.is_error()) {
      FDF_LOG(ERROR, "Could not create WlanFullmacImplIfcBridge endpoints: %s",
              endpoints.status_string());
      completer.Reply(endpoints.take_error());
      return;
    }

    // Create and bind fic bridge server.
    ifc_bridge_server_ = std::make_unique<WlanFullmacImplIfcToDriverBridge>(
        driver_dispatcher_, std::move(endpoints->server), std::move(request.ifc()));

    bridge_client_->Start(std::move(endpoints->client))
        .ThenExactlyOnce([completer = completer.ToAsync()](
                             fidl::Result<WlanFullmacImplBridge::Start>& result) mutable {
          WLAN_LAMBDA_TRACE_DURATION("WlanFullmacImplBridge::Start callback");
          // Unlike all the other methods in WlanFullmacImpl,
          // WlanFullmacImpl::Start and WlanFullmacImplBridge::Start results are
          // considered different types by the compiler. So we need to convert between the two
          // types manually here.
          if (result.is_error()) {
            auto& error = result.error_value();
            FDF_SLOG(ERROR, "Start failed", KV("status", error.FormatDescription()));
            completer.Reply(zx::error(FidlErrorToStatus(error)));
          } else {
            // Forward the SME channel provided by test suite
            completer.Reply(zx::ok(std::move(result->sme_channel())));
          }
        });
  }

  void Stop(StopCompleter::Sync& completer) override {}
  void Query(QueryCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->Query().ThenExactlyOnce(
        ForwardResult<WlanFullmacImplBridge::Query>(completer.ToAsync()));
  }
  void QueryMacSublayerSupport(QueryMacSublayerSupportCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->QueryMacSublayerSupport().ThenExactlyOnce(
        ForwardResult<WlanFullmacImplBridge::QueryMacSublayerSupport>(completer.ToAsync()));
  }
  void QuerySecuritySupport(QuerySecuritySupportCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->QuerySecuritySupport().ThenExactlyOnce(
        ForwardResult<WlanFullmacImplBridge::QuerySecuritySupport>(completer.ToAsync()));
  }
  void QuerySpectrumManagementSupport(
      QuerySpectrumManagementSupportCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->QuerySpectrumManagementSupport().ThenExactlyOnce(
        ForwardResult<WlanFullmacImplBridge::QuerySpectrumManagementSupport>(completer.ToAsync()));
  }
  void StartScan(StartScanRequest& request, StartScanCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->StartScan(request).ThenExactlyOnce(
        ForwardResult<WlanFullmacImplBridge::StartScan>(completer.ToAsync()));
  }
  void Connect(ConnectRequest& request, ConnectCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    bridge_client_->Connect(request).ThenExactlyOnce(
        ForwardResult<WlanFullmacImplBridge::Connect>(completer.ToAsync()));
  }
  void Reconnect(ReconnectRequest& request, ReconnectCompleter::Sync& completer) override {}
  void AuthResp(AuthRespRequest& request, AuthRespCompleter::Sync& completer) override {}
  void Deauth(DeauthRequest& request, DeauthCompleter::Sync& completer) override {}
  void AssocResp(AssocRespRequest& request, AssocRespCompleter::Sync& completer) override {}
  void Disassoc(DisassocRequest& request, DisassocCompleter::Sync& completer) override {}
  void Reset(ResetRequest& request, ResetCompleter::Sync& completer) override {}
  void StartBss(StartBssRequest& request, StartBssCompleter::Sync& completer) override {}
  void StopBss(StopBssRequest& request, StopBssCompleter ::Sync& completer) override {}
  void SetKeysReq(SetKeysReqRequest& request, SetKeysReqCompleter::Sync& completer) override {}
  void DelKeysReq(DelKeysReqRequest& request, DelKeysReqCompleter::Sync& completer) override {}
  void EapolTx(EapolTxRequest& request, EapolTxCompleter::Sync& completer) override {}
  void GetIfaceCounterStats(GetIfaceCounterStatsCompleter::Sync& completer) override {}
  void GetIfaceHistogramStats(GetIfaceHistogramStatsCompleter::Sync& completer) override {}
  void SetMulticastPromisc(SetMulticastPromiscRequest& request,
                           SetMulticastPromiscCompleter::Sync& completer) override {}
  void SaeHandshakeResp(SaeHandshakeRespRequest& request,
                        SaeHandshakeRespCompleter::Sync& completer) override {}
  void SaeFrameTx(SaeFrameTxRequest& request, SaeFrameTxCompleter::Sync& completer) override {}
  void WmmStatusReq(WmmStatusReqCompleter::Sync& completer) override {}
  void OnLinkStateChanged(OnLinkStateChangedRequest& request,
                          OnLinkStateChangedCompleter::Sync& completer) override {}

 private:
  fdf::ServerBinding<fuchsia_wlan_fullmac::WlanFullmacImpl> binding_;
  fidl::Client<fuchsia_wlan_fullmac::WlanFullmacImplBridge> bridge_client_;
  fdf_dispatcher_t* driver_dispatcher_{};
  std::unique_ptr<WlanFullmacImplIfcToDriverBridge> ifc_bridge_server_;
};

class TestController : public fdf::DriverBase,
                       public fidl::Server<test_wlan_testcontroller::TestController> {
  static constexpr std::string_view kDriverName = "wlan_testcontroller";

 public:
  TestController(fdf::DriverStartArgs start_args,
                 fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)),
        devfs_connector_(bindings_.CreateHandler(this, dispatcher(), fidl::kIgnoreBindingClosure)) {
  }

  zx::result<> Start() override {
    WLAN_TRACE_DURATION();
    node_.Bind(std::move(node()));
    fidl::Arena arena;

    zx::result connector = devfs_connector_.Bind(dispatcher());
    if (connector.is_error()) {
      return connector.take_error();
    }

    // By calling AddChild with devfs_args, the child driver will be discoverable through devfs.
    auto args =
        fuchsia_driver_framework::NodeAddArgs({.name = std::string(kDriverName),
                                               .devfs_args = fuchsia_driver_framework::DevfsAddArgs(
                                                   {.connector = std::move(connector.value())})});

    auto controller_endpoints = fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

    auto result = node_->AddChild({std::move(args), std::move(controller_endpoints.server), {}});
    if (result.is_error()) {
      auto& error = result.error_value();
      FDF_SLOG(ERROR, "Failed to add child", KV("status", error.FormatDescription()));

      // AddChild's domain error is a NodeError, not a zx_status_t
      return zx::error(error.is_domain_error() ? ZX_ERR_INTERNAL
                                               : error.framework_error().status());
    }

    return zx::ok();
  }

  void CreateFullmac(CreateFullmacRequest& request,
                     CreateFullmacCompleter::Sync& completer) override {
    WLAN_TRACE_DURATION();
    auto protocol_handler =
        [this, bridge_client = std::move(request.bridge_client())](
            fdf::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> server_end) mutable {
          WLAN_LAMBDA_TRACE_DURATION("WlanFullmacImpl::Service protocol handler");
          auto impl = std::make_unique<WlanFullmacImplToChannelBridge>(
              driver_dispatcher()->get(), std::move(server_end), std::move(bridge_client));
          fullmac_bridges_.push_back(std::move(impl));
        };

    fuchsia_wlan_fullmac::Service::InstanceHandler handler(
        {.wlan_fullmac_impl = std::move(protocol_handler)});

    // Generate a unique child name for the new fullmac child.
    std::stringstream ss;
    ss << "fullmac-child-" << num_fullmac_children_++;
    std::string child_name = ss.str();

    zx::result result =
        outgoing()->AddService<fuchsia_wlan_fullmac::Service>(std::move(handler), child_name);
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to add fullmac service: %s", result.status_string());
      completer.Reply(result.take_error());
      return;
    }

    auto offers = std::vector{fdf::MakeOffer2<fuchsia_wlan_fullmac::Service>(child_name)};

    auto args = fuchsia_driver_framework::NodeAddArgs({
        .name = child_name,
        .offers2 = std::move(offers),
    });

    zx::result controller_endpoints =
        fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
    ZX_ASSERT(controller_endpoints.is_ok());

    auto add_child_result =
        node_->AddChild({std::move(args), std::move(controller_endpoints->server), {}});
    if (add_child_result.is_error()) {
      auto& error = add_child_result.error_value();
      FDF_SLOG(ERROR, "Failed to add child", KV("status", error.FormatDescription()));

      // AddChild's domain error is a NodeError, not a zx_status_t
      completer.Reply(zx::error(
          (error.is_domain_error() ? ZX_ERR_INTERNAL : error.framework_error().status())));
      return;
    }
    completer.Reply(zx::ok());
  }

 private:
  fidl::SyncClient<fuchsia_driver_framework::Node> node_;

  // Holds bindings to TestController, which all bind to this class
  fidl::ServerBindingGroup<test_wlan_testcontroller::TestController> bindings_;

  // devfs_connector_ lets the class serve the TestController protocol over devfs.
  driver_devfs::Connector<test_wlan_testcontroller::TestController> devfs_connector_;

  // Tracks the number of fullmac client and aps created. Used to generate names.
  int num_fullmac_children_ = 0;

  std::vector<std::unique_ptr<WlanFullmacImplToChannelBridge>> fullmac_bridges_;
};

}  // namespace wlan_testcontroller

FUCHSIA_DRIVER_EXPORT(wlan_testcontroller::TestController);
