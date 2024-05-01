// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "softmac_bridge.h"

#include <fidl/fuchsia.wlan.softmac/cpp/driver/fidl.h>
#include <fidl/fuchsia.wlan.softmac/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fdf/dispatcher.h>
#include <lib/fit/function.h>
#include <lib/sync/cpp/completion.h>
#include <lib/trace/event.h>
#include <zircon/status.h>

#include <wlan/drivers/fidl_bridge.h>
#include <wlan/drivers/log.h>

#include "softmac_ifc_bridge.h"
#include "src/connectivity/wlan/drivers/wlansoftmac/rust_driver/c-binding/bindings.h"

namespace wlan::drivers::wlansoftmac {

using ::wlan::drivers::fidl_bridge::FidlErrorToStatus;
using ::wlan::drivers::fidl_bridge::ForwardResult;

SoftmacBridge::SoftmacBridge(fdf::SharedClient<fuchsia_wlan_softmac::WlanSoftmac>&& softmac_client,
                             std::shared_ptr<std::mutex> ethernet_proxy_lock,
                             ddk::EthernetIfcProtocolClient* ethernet_proxy,
                             std::optional<uint32_t>* cached_ethernet_status)
    : softmac_client_(
          std::forward<fdf::SharedClient<fuchsia_wlan_softmac::WlanSoftmac>>(softmac_client)),
      ethernet_proxy_lock_(std::move(ethernet_proxy_lock)),
      ethernet_proxy_(ethernet_proxy),
      cached_ethernet_status_(cached_ethernet_status) {
  WLAN_TRACE_DURATION();
  auto rust_dispatcher = fdf::SynchronizedDispatcher::Create(
      fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "bridged-wlansoftmac",
      [](fdf_dispatcher_t* rust_dispatcher) {
        WLAN_LAMBDA_TRACE_DURATION("rust_dispatcher shutdown_handler");
        fdf_dispatcher_destroy(rust_dispatcher);
      });
  if (rust_dispatcher.is_error()) {
    ZX_ASSERT_MSG(false, "Failed to create dispatcher for MLME: %s",
                  zx_status_get_string(rust_dispatcher.status_value()));
  }
  rust_dispatcher_ = *std::move(rust_dispatcher);
}

SoftmacBridge::~SoftmacBridge() {
  WLAN_TRACE_DURATION();
  ldebug(0, nullptr, "Entering.");
  rust_dispatcher_.ShutdownAsync();
  // The provided ShutdownHandler will call fdf_dispatcher_destroy().
  rust_dispatcher_.release();
}

zx::result<std::unique_ptr<SoftmacBridge>> SoftmacBridge::New(
    std::unique_ptr<fit::callback<void(zx_status_t status)>> completer,
    fit::callback<void(zx_status_t)> sta_shutdown_handler,
    fdf::SharedClient<fuchsia_wlan_softmac::WlanSoftmac>&& softmac_client,
    std::shared_ptr<std::mutex> ethernet_proxy_lock, ddk::EthernetIfcProtocolClient* ethernet_proxy,
    std::optional<uint32_t>* cached_ethernet_status) {
  WLAN_TRACE_DURATION();
  auto softmac_bridge = std::unique_ptr<SoftmacBridge>(
      new SoftmacBridge(std::move(softmac_client), std::move(ethernet_proxy_lock), ethernet_proxy,
                        cached_ethernet_status));

  auto softmac_bridge_endpoints = fidl::CreateEndpoints<fuchsia_wlan_softmac::WlanSoftmacBridge>();
  if (softmac_bridge_endpoints.is_error()) {
    lerror("Failed to create WlanSoftmacBridge endpoints: %s",
           softmac_bridge_endpoints.status_string());
    return softmac_bridge_endpoints.take_error();
  }

  {
    WLAN_LAMBDA_TRACE_DURATION("WlanSoftmacBridge server binding");
    softmac_bridge->softmac_bridge_server_ =
        std::make_unique<fidl::ServerBinding<fuchsia_wlan_softmac::WlanSoftmacBridge>>(
            fdf::Dispatcher::GetCurrent()->async_dispatcher(),
            std::move(softmac_bridge_endpoints->server), softmac_bridge.get(),
            [](fidl::UnbindInfo info) {
              WLAN_LAMBDA_TRACE_DURATION("WlanSoftmacBridge close_handler");
              if (info.is_user_initiated()) {
                linfo("WlanSoftmacBridge server closed.");
              } else {
                lerror("WlanSoftmacBridge unexpectedly closed: %s", info.lossy_description());
              }
            });
  }

  auto init_completer = std::make_unique<InitCompleter>(
      [softmac_bridge = softmac_bridge.get(),
       completer = std::move(completer)](zx_status_t status) mutable {
        WLAN_LAMBDA_TRACE_DURATION("SoftmacBridge startup_rust_completer");
        (*completer)(status);
      });

  async::PostTask(softmac_bridge->rust_dispatcher_.async_dispatcher(),
                  [init_completer = std::move(init_completer),
                   sta_shutdown_handler = std::move(sta_shutdown_handler),
                   frame_sender =
                       frame_sender_t{
                           .ctx = softmac_bridge.get(),
                           .wlan_tx = &SoftmacBridge::WlanTx,
                           .ethernet_rx = &SoftmacBridge::EthernetRx,
                       },
                   rust_buffer_provider = softmac_bridge->rust_buffer_provider,
                   softmac_bridge_client_end =
                       softmac_bridge_endpoints->client.TakeHandle().release()]() mutable {
                    WLAN_LAMBDA_TRACE_DURATION("Rust MLME dispatcher");
                    sta_shutdown_handler(start_and_run_bridged_wlansoftmac(
                        init_completer.release(),
                        [](void* ctx, zx_status_t status) {
                          WLAN_LAMBDA_TRACE_DURATION("run InitCompleter");
                          // Safety: `init_completer` is now owned by this function, so it's safe to
                          // cast it to a non-const pointer.
                          auto init_completer = static_cast<InitCompleter*>(ctx);
                          if (init_completer == nullptr) {
                            lerror("Received NULL InitCompleter pointer!");
                            return;
                          }
                          // Skip the check for whether completer has already been
                          // called.  This is the only location where completer is
                          // called, and its deallocated immediately after. Thus, such a
                          // check would be a use-after-free violation.
                          (*init_completer)(status);
                          delete init_completer;
                        },
                        frame_sender, rust_buffer_provider, softmac_bridge_client_end));
                  });

  return fit::success(std::move(softmac_bridge));
}

void SoftmacBridge::StopBridgedDriver(std::unique_ptr<fit::callback<void()>> stop_completer) {
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
  debugf("Start");

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    completer.Reply(fit::error(ZX_ERR_INTERNAL));
    return;
  }

  auto endpoints = fdf::CreateEndpoints<fuchsia_wlan_softmac::WlanSoftmacIfc>();
  if (endpoints.is_error()) {
    lerror("Creating end point error: %s", endpoints.status_string());
    completer.Reply(fit::error(endpoints.status_value()));
    return;
  }

  auto softmac_ifc_bridge_client_endpoint = std::move(request.ifc_bridge());

  // See the `fuchsia.wlan.mlme/WlanSoftmacBridge.Start` documentation about the FFI
  // provided by the `frame_processor` field.
  auto frame_processor =
      reinterpret_cast<const frame_processor_t*>(  // NOLINT(performance-no-int-to-ptr)
          request.frame_processor());

  auto softmac_ifc_bridge = SoftmacIfcBridge::New(frame_processor, std::move(endpoints->server),
                                                  std::move(softmac_ifc_bridge_client_endpoint));

  if (softmac_ifc_bridge.is_error()) {
    lerror("Failed to create SoftmacIfcBridge: %s", softmac_ifc_bridge.status_string());
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
          auto error = result.error_value();
          lerror("Failed getting start result (FIDL error %s)", error);
          completer.Reply(fit::error(FidlErrorToStatus(error)));
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
  auto fidl_request = fidl::Unpersist<fuchsia_wlan_softmac::FrameSenderWlanTxRequest>(
      cpp20::span(payload, payload_size));
  if (!fidl_request.is_ok()) {
    lerror("Failed to unpersist FrameSender.WlanTxRequest: %s", fidl_request.error_value());
    return ZX_ERR_INTERNAL;
  }

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return ZX_ERR_INTERNAL;
  }

  if (!fidl_request->async_id()) {
    lerror("QueueWlanTx request missing async_id field.");
    return ZX_ERR_INTERNAL;
  }
  auto async_id = fidl_request->async_id().value();

  if (!fidl_request->packet_address() || !fidl_request->packet_size() ||
      !fidl_request->packet_info()) {
    lerror("QueueWlanTx request missing required field(s).");
    auto status = ZX_ERR_INTERNAL;
    WLAN_TRACE_ASYNC_END_TX(async_id, status);
    return status;
  }

  auto buffer = reinterpret_cast<Buffer*>(  // NOLINT(performance-no-int-to-ptr)
      fidl_request->packet_address().value());
  if (buffer == nullptr) {
    lerror("QueueWlanTx contains NULL packet address.");
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
          lerror("Failed to queue frame in the vendor driver: %s", zx_status_get_string(status));
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
  auto fidl_request = fidl::Unpersist<fuchsia_wlan_softmac::FrameSenderEthernetRxRequest>(
      cpp20::span(payload, payload_size));
  if (!fidl_request.is_ok()) {
    lerror("Failed to unpersist FrameSender.EthernetRx request: %s", fidl_request.error_value());
    return ZX_ERR_INTERNAL;
  }

  if (!fidl_request->packet_address() || !fidl_request->packet_size()) {
    lerror("FrameSender.EthernetRx request missing required field(s).");
    return ZX_ERR_INTERNAL;
  }

  if (fidl_request->packet_size().value() > ETH_FRAME_MAX_SIZE) {
    lerror("Attempted to deliver an ethernet frame of invalid length: %zu",
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
      .raw = released_buffer,
      .data = released_buffer->data(),
      .capacity = released_buffer->capacity(),
  };
}

}  // namespace wlan::drivers::wlansoftmac
