// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "softmac_bridge.h"

#include <fidl/fuchsia.driver.framework/cpp/driver/fidl.h>
#include <fidl/fuchsia.wlan.softmac/cpp/driver/fidl.h>
#include <fidl/fuchsia.wlan.softmac/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/driver/component/cpp/start_completer.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fdf/dispatcher.h>
#include <lib/fit/function.h>
#include <lib/sync/cpp/completion.h>
#include <lib/trace-engine/types.h>
#include <lib/zx/result.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <wlan/drivers/fidl_bridge.h>
#include <wlan/drivers/log.h>

#include "softmac_ifc_bridge.h"
#include "src/connectivity/wlan/drivers/wlansoftmac/rust_driver/c-binding/bindings.h"

namespace wlan::drivers::wlansoftmac {

using ::wlan::drivers::fidl_bridge::FidlErrorToStatus;
using ::wlan::drivers::fidl_bridge::ForwardResult;

SoftmacBridge::SoftmacBridge(fidl::SharedClient<fuchsia_driver_framework::Node> node_client,
                             fdf::SharedClient<fuchsia_wlan_softmac::WlanSoftmac>&& softmac_client,
                             std::shared_ptr<std::mutex> ethernet_proxy_lock,
                             ddk::EthernetIfcProtocolClient* ethernet_proxy,
                             std::optional<uint32_t>* cached_ethernet_status)
    : node_client_(std::move(node_client)),
      softmac_client_(std::move(softmac_client)),
      ethernet_proxy_lock_(std::move(ethernet_proxy_lock)),
      ethernet_proxy_(ethernet_proxy),
      cached_ethernet_status_(cached_ethernet_status) {
  WLAN_TRACE_DURATION();
}

SoftmacBridge::~SoftmacBridge() { WLAN_TRACE_DURATION(); }

// Start the bridged wlansoftmac driver and forward calls from that driver through this one.
//
// This function starts the bridged driver by posting a task to the current dispatcher.
// The provided `sta_shutdown_handler` will be called on the return value of
// `wlansoftmac_c::start_and_run_bridged_wlansoftmac` called in the posted task.
//
// Note that `wlansoftmac_c::start_and_run_bridged_wlansoftmac` may exit with an error during
// startup of the bridged driver. Thus, `sta_shutdown_handler` cannot depend on any operations
// performed by the bridged driver. In general, the `sta_shutdown_handler` should cause this
// driver to stop if `wlansoftmac_c::start_and_run_bridged_wlansoftmac` returns an error because
// this driver is in a defunct state without a bridged driver.
zx::result<std::unique_ptr<SoftmacBridge>> SoftmacBridge::New(
    fidl::SharedClient<fuchsia_driver_framework::Node> node_client, fdf::StartCompleter completer,
    fit::callback<void(zx_status_t)> sta_shutdown_handler,
    fdf::SharedClient<fuchsia_wlan_softmac::WlanSoftmac>&& softmac_client,
    std::shared_ptr<std::mutex> ethernet_proxy_lock, ddk::EthernetIfcProtocolClient* ethernet_proxy,
    std::optional<uint32_t>* cached_ethernet_status) {
  WLAN_TRACE_DURATION();
  auto softmac_bridge = std::unique_ptr<SoftmacBridge>(
      new SoftmacBridge(std::move(node_client), std::move(softmac_client),
                        std::move(ethernet_proxy_lock), ethernet_proxy, cached_ethernet_status));

  auto softmac_bridge_endpoints = fidl::CreateEndpoints<fuchsia_wlan_softmac::WlanSoftmacBridge>();
  if (softmac_bridge_endpoints.is_error()) {
    FDF_LOG(ERROR, "Failed to create WlanSoftmacBridge endpoints: %s",
            softmac_bridge_endpoints.status_string());
    completer(zx::error(softmac_bridge_endpoints.error_value()));
    return softmac_bridge_endpoints.take_error();
  }

  {
    WLAN_LAMBDA_TRACE_DURATION("WlanSoftmacBridge server binding");
    softmac_bridge->softmac_bridge_server_ =
        std::make_unique<fidl::ServerBinding<fuchsia_wlan_softmac::WlanSoftmacBridge>>(
            fdf::Dispatcher::GetCurrent()->async_dispatcher(),
            std::move(softmac_bridge_endpoints->server), softmac_bridge.get(),
            [node_client = softmac_bridge->node_client_.Clone()](fidl::UnbindInfo info) mutable {
              WLAN_LAMBDA_TRACE_DURATION("WlanSoftmacBridge close_handler");
              if (info.is_user_initiated()) {
                FDF_LOG(INFO, "WlanSoftmacBridge server closed.");
              } else {
                FDF_LOG(ERROR, "WlanSoftmacBridge unexpectedly closed: %s",
                        info.lossy_description());

                // Initiate asynchronous teardown of the fuchsia.driver.framework/Node proxy
                // to cause the driver framework to stop this driver. Stopping this driver is
                // appropriate when the bridge has an abnormal shutdown because otherwise this
                // driver would be in an unusable state.
                node_client.AsyncTeardown();
              }
            });
  }

  auto start_completer = std::make_unique<fit::callback<void(zx_status_t)>>(
      [completer = std::move(completer)](zx_status_t status) mutable {
        WLAN_LAMBDA_TRACE_DURATION("bridged_start_completer");
        completer(zx::make_result(status));
      });

  FDF_LOG(INFO, "Starting up Rust WlanSoftmac...");
  auto rust_dispatcher = fdf::SynchronizedDispatcher::Create(
      fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "bridged-wlansoftmac",
      [](fdf_dispatcher_t* rust_dispatcher) {
        WLAN_LAMBDA_TRACE_DURATION("bridged-wlansoftmac dispatcher shutdown_handler");
        FDF_LOG(INFO, "Completed bridged-wlansoftmac dispatcher shutdown");
      });

  if (rust_dispatcher.is_error()) {
    FDF_LOG(ERROR, "Failed to create dispatcher for MLME: %s", rust_dispatcher.status_string());
    completer(zx::error(rust_dispatcher.error_value()));
    return rust_dispatcher.take_error();
  }

  // Release the dispatcher to defer destruction until driver shutdown. The framework will shutdown
  // the dispatcher despite its release here because the framework tagged this driver as the owner
  // of the dispatcher upon creation. The framework always automatically shuts down all dispatchers
  // owned by a driver during the driver's shut down.
  //
  // A reference to the dispatcher is kept for the purpose of posting a single task to run the
  // bridged driver.
  auto unowned_rust_dispatcher = rust_dispatcher->borrow();
  rust_dispatcher->release();

  async::PostTask(unowned_rust_dispatcher->async_dispatcher(),
                  [sta_shutdown_handler = std::move(sta_shutdown_handler),
                   start_completer = std::move(start_completer),
                   ethernet_rx =
                       ethernet_rx_t{
                           .ctx = softmac_bridge.get(),
                           .transfer = &SoftmacBridge::EthernetRx,
                       },
                   wlan_tx =
                       wlan_tx_t{
                           .ctx = softmac_bridge.get(),
                           .transfer = &SoftmacBridge::WlanTx,
                       },
                   rust_buffer_provider = softmac_bridge->rust_buffer_provider,
                   softmac_bridge_client_end =
                       softmac_bridge_endpoints->client.TakeHandle().release()]() mutable {
                    WLAN_LAMBDA_TRACE_DURATION("Rust MLME dispatcher");
                    sta_shutdown_handler(start_and_run_bridged_wlansoftmac(
                        start_completer.release(),
                        [](void* start_completer, zx_status_t status) {
                          WLAN_LAMBDA_TRACE_DURATION("start_completer");
                          ZX_ASSERT_MSG(start_completer != nullptr,
                                        "Received NULL start_completer.");
                          // Safety: `start_completer` is now owned by this function, so it's safe
                          // to cast it to a non-const pointer.
                          auto start_completer_cb =
                              static_cast<fit::callback<void(zx_status_t)>*>(start_completer);
                          // Skip the check for whether completer has already been
                          // called.  This is the only location where completer is
                          // called, and its deallocated immediately after. Thus, such a
                          // check would be a use-after-free violation.
                          (*start_completer_cb)(status);
                          delete start_completer_cb;
                        },
                        ethernet_rx, wlan_tx, rust_buffer_provider, softmac_bridge_client_end));
                  });

  return fit::success(std::move(softmac_bridge));
}

void SoftmacBridge::StopBridgedDriver(fit::callback<void()> stop_completer) {
  WLAN_TRACE_DURATION();
  softmac_ifc_bridge_->StopBridgedDriver(std::move(stop_completer));
}

void SoftmacBridge::Query(QueryCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  softmac_client_->Query().Then(
      ForwardResult<fuchsia_wlan_softmac::WlanSoftmac::Query>(completer.ToAsync()));
}

void SoftmacBridge::QueryDiscoverySupport(QueryDiscoverySupportCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  softmac_client_->QueryDiscoverySupport().Then(
      ForwardResult<fuchsia_wlan_softmac::WlanSoftmac::QueryDiscoverySupport>(completer.ToAsync()));
}

void SoftmacBridge::QueryMacSublayerSupport(QueryMacSublayerSupportCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  softmac_client_->QueryMacSublayerSupport().Then(
      ForwardResult<fuchsia_wlan_softmac::WlanSoftmac::QueryMacSublayerSupport>(
          completer.ToAsync()));
}

void SoftmacBridge::QuerySecuritySupport(QuerySecuritySupportCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  softmac_client_->QuerySecuritySupport().Then(
      ForwardResult<fuchsia_wlan_softmac::WlanSoftmac::QuerySecuritySupport>(completer.ToAsync()));
}

void SoftmacBridge::QuerySpectrumManagementSupport(
    QuerySpectrumManagementSupportCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  softmac_client_->QuerySpectrumManagementSupport().Then(
      ForwardResult<fuchsia_wlan_softmac::WlanSoftmac::QuerySpectrumManagementSupport>(
          completer.ToAsync()));
}

void SoftmacBridge::Start(StartRequest& request, StartCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();

  auto endpoints = fdf::CreateEndpoints<fuchsia_wlan_softmac::WlanSoftmacIfc>();
  if (endpoints.is_error()) {
    FDF_LOG(ERROR, "Creating end point error: %s", endpoints.status_string());
    completer.Reply(fit::error(endpoints.status_value()));
    return;
  }

  auto softmac_ifc_bridge_client_endpoint = std::move(request.ifc_bridge());

  // See the `fuchsia.wlan.mlme/WlanSoftmacBridge.Start` documentation about the FFI
  // provided by the `ethernet_tx` field.
  auto ethernet_tx = reinterpret_cast<const ethernet_tx_t*>(  // NOLINT(performance-no-int-to-ptr)
      request.ethernet_tx());
  // See the `fuchsia.wlan.mlme/WlanSoftmacBridge.Start` documentation about the FFI
  // provided by the `wlan_rx` field.
  auto wlan_rx = reinterpret_cast<const wlan_rx_t*>(  // NOLINT(performance-no-int-to-ptr)
      request.wlan_rx());

  auto softmac_ifc_bridge = SoftmacIfcBridge::New(node_client_.Clone(), ethernet_tx, wlan_rx,
                                                  std::move(endpoints->server),
                                                  std::move(softmac_ifc_bridge_client_endpoint));

  if (softmac_ifc_bridge.is_error()) {
    FDF_LOG(ERROR, "Failed to create SoftmacIfcBridge: %s", softmac_ifc_bridge.status_string());
    completer.Reply(fit::error(softmac_ifc_bridge.status_value()));
    return;
  }
  softmac_ifc_bridge_ = *std::move(softmac_ifc_bridge);

  fidl::Request<fuchsia_wlan_softmac::WlanSoftmac::Start> fdf_request;
  fdf_request.ifc(std::move(endpoints->client));
  softmac_client_->Start(std::move(fdf_request))
      .Then([completer = completer.ToAsync()](
                fdf::Result<fuchsia_wlan_softmac::WlanSoftmac::Start>& result) mutable {
        if (result.is_error()) {
          auto status = FidlErrorToStatus(result.error_value());
          FDF_LOG(ERROR, "Failed getting start result (FIDL error %s)",
                  zx_status_get_string(status));
          completer.Reply(fit::error(status));
        } else {
          fuchsia_wlan_softmac::WlanSoftmacBridgeStartResponse fidl_response(
              std::move(result.value().sme_channel()));
          completer.Reply(fit::ok(std::move(fidl_response)));
        }
      });
}

void SoftmacBridge::SetEthernetStatus(SetEthernetStatusRequest& request,
                                      SetEthernetStatusCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  std::lock_guard<std::mutex> lock(*ethernet_proxy_lock_);
  if (ethernet_proxy_->is_valid()) {
    ethernet_proxy_->Status(request.status());
  } else {
    *cached_ethernet_status_ = request.status();
  }
  completer.Reply();
}

void SoftmacBridge::SetChannel(SetChannelRequest& request, SetChannelCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  softmac_client_->SetChannel(request).Then(
      ForwardResult<fuchsia_wlan_softmac::WlanSoftmac::SetChannel>(completer.ToAsync()));
}

void SoftmacBridge::JoinBss(JoinBssRequest& request, JoinBssCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  softmac_client_->JoinBss(request).Then(
      ForwardResult<fuchsia_wlan_softmac::WlanSoftmac::JoinBss>(completer.ToAsync()));
}

void SoftmacBridge::EnableBeaconing(EnableBeaconingRequest& request,
                                    EnableBeaconingCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  softmac_client_->EnableBeaconing(request).Then(
      ForwardResult<fuchsia_wlan_softmac::WlanSoftmac::EnableBeaconing>(completer.ToAsync()));
}

void SoftmacBridge::DisableBeaconing(DisableBeaconingCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  softmac_client_->DisableBeaconing().Then(
      ForwardResult<fuchsia_wlan_softmac::WlanSoftmac::DisableBeaconing>(completer.ToAsync()));
}

void SoftmacBridge::InstallKey(InstallKeyRequest& request, InstallKeyCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  softmac_client_->InstallKey(request).Then(
      ForwardResult<fuchsia_wlan_softmac::WlanSoftmac::InstallKey>(completer.ToAsync()));
}

void SoftmacBridge::NotifyAssociationComplete(NotifyAssociationCompleteRequest& request,
                                              NotifyAssociationCompleteCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  softmac_client_->NotifyAssociationComplete(request).Then(
      ForwardResult<fuchsia_wlan_softmac::WlanSoftmac::NotifyAssociationComplete>(
          completer.ToAsync()));
}

void SoftmacBridge::ClearAssociation(ClearAssociationRequest& request,
                                     ClearAssociationCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  softmac_client_->ClearAssociation(request).Then(
      ForwardResult<fuchsia_wlan_softmac::WlanSoftmac::ClearAssociation>(completer.ToAsync()));
}

void SoftmacBridge::StartPassiveScan(StartPassiveScanRequest& request,
                                     StartPassiveScanCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  softmac_client_->StartPassiveScan(request).Then(
      ForwardResult<fuchsia_wlan_softmac::WlanSoftmac::StartPassiveScan>(completer.ToAsync()));
}

void SoftmacBridge::StartActiveScan(StartActiveScanRequest& request,
                                    StartActiveScanCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  softmac_client_->StartActiveScan(request).Then(
      ForwardResult<fuchsia_wlan_softmac::WlanSoftmac::StartActiveScan>(completer.ToAsync()));
}

void SoftmacBridge::CancelScan(CancelScanRequest& request, CancelScanCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  softmac_client_->CancelScan(request).Then(
      ForwardResult<fuchsia_wlan_softmac::WlanSoftmac::CancelScan>(completer.ToAsync()));
}

void SoftmacBridge::UpdateWmmParameters(UpdateWmmParametersRequest& request,
                                        UpdateWmmParametersCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  softmac_client_->UpdateWmmParameters(request).Then(
      ForwardResult<fuchsia_wlan_softmac::WlanSoftmac::UpdateWmmParameters>(completer.ToAsync()));
}

zx::result<> SoftmacBridge::EthernetTx(eth::BorrowedOperation<>* op,
                                       trace_async_id_t async_id) const {
  return softmac_ifc_bridge_->EthernetTx(op, async_id);
}

// Queues a packet for transmission.
//
// The returned status only indicates whether `SoftmacBridge` successfully queued the
// packet and not that the packet was successfully sent.
zx_status_t SoftmacBridge::WlanTx(void* ctx, const uint8_t* payload, size_t payload_size) {
  auto self = static_cast<const SoftmacBridge*>(ctx);

  WLAN_TRACE_DURATION();
  auto fidl_request = fidl::Unpersist<fuchsia_wlan_softmac::WlanTxTransferRequest>(
      cpp20::span(payload, payload_size));
  if (!fidl_request.is_ok()) {
    FDF_LOG(ERROR, "Failed to unpersist WlanTx.Transfer request: %s",
            fidl_request.error_value().status_string());
    return ZX_ERR_INTERNAL;
  }

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    FDF_LOG(ERROR, "Arena creation failed: %s", arena.status_string());
    return ZX_ERR_INTERNAL;
  }

  if (!fidl_request->async_id()) {
    FDF_LOG(ERROR, "QueueWlanTx request missing async_id field.");
    return ZX_ERR_INTERNAL;
  }
  auto async_id = fidl_request->async_id().value();

  if (!fidl_request->packet_address() || !fidl_request->packet_size() ||
      !fidl_request->packet_info()) {
    FDF_LOG(ERROR, "QueueWlanTx request missing required field(s).");
    auto status = ZX_ERR_INTERNAL;
    WLAN_TRACE_ASYNC_END_TX(async_id, status);
    return status;
  }

  auto buffer = reinterpret_cast<Buffer*>(  // NOLINT(performance-no-int-to-ptr)
      fidl_request->packet_address().value());
  if (buffer == nullptr) {
    FDF_LOG(ERROR, "QueueWlanTx contains NULL packet address.");
    auto status = ZX_ERR_INTERNAL;
    WLAN_TRACE_ASYNC_END_TX(async_id, status);
    return status;
  }

  auto finalized_buffer = FinalizedBuffer(buffer, fidl_request->packet_size().value());
  ZX_DEBUG_ASSERT(finalized_buffer.written() <= std::numeric_limits<uint16_t>::max());

  fuchsia_wlan_softmac::WlanTxPacket fdf_request;
  fdf_request.mac_frame(::std::vector<uint8_t>(
      finalized_buffer.data(), finalized_buffer.data() + finalized_buffer.written()));
  fdf_request.info(fidl_request->packet_info().value());

  // Queue the frame to be sent by the vendor driver, but don't block this thread on
  // the returned status. Supposing an error preventing transmission beyond this point
  // occurred, MLME would handle the failure the same as other transmission failures
  // inherent in the 802.11 protocol itself. In general, MLME cannot soley rely on
  // detecting transmission failures via this function's return value, so it's
  // unnecessary to block the MLME thread any longer once the well-formed packet has
  // been sent to the vendor driver.
  //
  // Unlike other error logging above, it's critical this callback logs an error
  // if there is one because the error may otherwise be silently discarded since
  // MLME will not receive the error.
  self->softmac_client_->QueueTx(fdf_request)
      .Then([loc = cpp20::source_location::current(),
             async_id](fdf::Result<fuchsia_wlan_softmac::WlanSoftmac::QueueTx>& result) mutable {
        if (result.is_error()) {
          auto status = FidlErrorToStatus(result.error_value());
          FDF_LOG(ERROR, "Failed to queue frame in the vendor driver: %s",
                  zx_status_get_string(status));
          WLAN_TRACE_ASYNC_END_TX(async_id, status);
        } else {
          WLAN_TRACE_ASYNC_END_TX(async_id, ZX_OK);
        }
      });

  return ZX_OK;
}

zx_status_t SoftmacBridge::EthernetRx(void* ctx, const uint8_t* payload, size_t payload_size) {
  auto self = static_cast<const SoftmacBridge*>(ctx);

  WLAN_TRACE_DURATION();
  auto fidl_request = fidl::Unpersist<fuchsia_wlan_softmac::EthernetRxTransferRequest>(
      cpp20::span(payload, payload_size));
  if (!fidl_request.is_ok()) {
    FDF_LOG(ERROR, "Failed to unpersist EthernetRx.Transfer request: %s",
            fidl_request.error_value().status_string());
    return ZX_ERR_INTERNAL;
  }

  if (!fidl_request->packet_address() || !fidl_request->packet_size()) {
    FDF_LOG(ERROR, "EthernetRx.Transfer request missing required field(s).");
    return ZX_ERR_INTERNAL;
  }

  if (fidl_request->packet_size().value() > ETH_FRAME_MAX_SIZE) {
    FDF_LOG(ERROR, "Attempted to deliver an ethernet frame of invalid length: %zu",
            fidl_request->packet_size().value());
    return ZX_ERR_INVALID_ARGS;
  }

  std::lock_guard<std::mutex> lock(*self->ethernet_proxy_lock_);
  if (self->ethernet_proxy_->is_valid()) {
    self->ethernet_proxy_->Recv(
        reinterpret_cast<const uint8_t*>(  // NOLINT(performance-no-int-to-ptr)
            fidl_request->packet_address().value()),
        fidl_request->packet_size().value(), 0u);
  }

  return ZX_OK;
}

wlansoftmac_buffer_t SoftmacBridge::IntoRustBuffer(std::unique_ptr<Buffer> buffer) {
  WLAN_TRACE_DURATION();
  auto* released_buffer = buffer.release();
  return wlansoftmac_buffer_t{
      .free =
          [](void* raw) {
            WLAN_LAMBDA_TRACE_DURATION("wlansoftmac_in_buf_t.free_buffer");
            std::unique_ptr<Buffer>(static_cast<Buffer*>(raw)).reset();
          },
      .ctx = released_buffer,
      .data = released_buffer->data(),
      .capacity = released_buffer->capacity(),
  };
}

}  // namespace wlan::drivers::wlansoftmac
