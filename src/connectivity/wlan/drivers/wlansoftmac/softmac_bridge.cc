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

#include "src/connectivity/wlan/drivers/wlansoftmac/rust_driver/c-binding/bindings.h"

namespace wlan::drivers::wlansoftmac {

using ::wlan::drivers::fidl_bridge::FidlErrorToStatus;
using ::wlan::drivers::fidl_bridge::ForwardResult;

SoftmacBridge::SoftmacBridge(DeviceInterface* device_interface,
                             fdf::SharedClient<fuchsia_wlan_softmac::WlanSoftmac>&& softmac_client,
                             std::shared_ptr<std::mutex> ethernet_proxy_lock,
                             ddk::EthernetIfcProtocolClient* ethernet_proxy)
    : softmac_client_(
          std::forward<fdf::SharedClient<fuchsia_wlan_softmac::WlanSoftmac>>(softmac_client)),
      device_interface_(device_interface),
      ethernet_proxy_lock_(std::move(ethernet_proxy_lock)),
      ethernet_proxy_(ethernet_proxy) {
  WLAN_TRACE_DURATION();
  auto rust_dispatcher = fdf::SynchronizedDispatcher::Create(
      fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "wlansoftmac-mlme",
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
    fdf::Dispatcher& softmac_bridge_server_dispatcher,
    std::unique_ptr<fit::callback<void(zx_status_t status)>> completer,
    fit::callback<void(zx_status_t)> sta_shutdown_handler, DeviceInterface* device,
    fdf::SharedClient<fuchsia_wlan_softmac::WlanSoftmac>&& softmac_client,
    std::shared_ptr<std::mutex> ethernet_proxy_lock,
    ddk::EthernetIfcProtocolClient* ethernet_proxy) {
  WLAN_TRACE_DURATION();
  auto softmac_bridge = std::unique_ptr<SoftmacBridge>(new SoftmacBridge(
      device, std::move(softmac_client), std::move(ethernet_proxy_lock), ethernet_proxy));

  // Safety: Each of the functions initialized in `wlansoftmac_rust_ops` is safe to call
  // from any thread as long they are never called concurrently.
  rust_device_interface_t wlansoftmac_rust_ops = {
      .device = static_cast<void*>(softmac_bridge->device_interface_),
      .start = [](void* device_interface, zx_handle_t softmac_ifc_bridge_client_handle,
                  const frame_processor_t* frame_processor,
                  zx_handle_t* out_sme_channel) -> zx_status_t {
        WLAN_LAMBDA_TRACE_DURATION("rust_device_interface_t.start");
        zx::channel channel;
        zx_status_t result =
            DeviceInterface::from(device_interface)
                ->Start(softmac_ifc_bridge_client_handle, frame_processor, &channel);
        *out_sme_channel = channel.release();
        return result;
      },
      .set_ethernet_status = [](void* device_interface, uint32_t status) -> zx_status_t {
        WLAN_LAMBDA_TRACE_DURATION("rust_device_interface_t.set_ethernet_status");
        return DeviceInterface::from(device_interface)->SetEthernetStatus(status);
      },
  };

  auto softmac_bridge_endpoints = fidl::CreateEndpoints<fuchsia_wlan_softmac::WlanSoftmacBridge>();
  if (softmac_bridge_endpoints.is_error()) {
    lerror("Failed to create WlanSoftmacBridge endpoints: %s",
           softmac_bridge_endpoints.status_string());
    return softmac_bridge_endpoints.take_error();
  }

  // Bind the WlanSoftmacBridge server on softmac_bridge_server_dispatcher. When the task
  // completes, it will signal server_binding_task_complete, primarily to indicate the captured
  // softmac_bridge pointer is no longer in use by the task.
  libsync::Completion server_binding_task_complete;
  async::PostTask(
      softmac_bridge_server_dispatcher.async_dispatcher(),
      [softmac_bridge = softmac_bridge.get(),
       softmac_bridge_server_endpoint = std::move(softmac_bridge_endpoints->server),
       &server_binding_task_complete]() mutable {
        WLAN_LAMBDA_TRACE_DURATION("WlanSoftmacBridge server binding");
        softmac_bridge->softmac_bridge_server_ =
            std::make_unique<fidl::ServerBinding<fuchsia_wlan_softmac::WlanSoftmacBridge>>(
                fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                std::move(softmac_bridge_server_endpoint), softmac_bridge,
                [](fidl::UnbindInfo info) {
                  WLAN_LAMBDA_TRACE_DURATION("WlanSoftmacBridge close_handler");
                  if (info.is_user_initiated()) {
                    linfo("WlanSoftmacBridge server closed.");
                  } else {
                    lerror("WlanSoftmacBridge unexpectedly closed: %s", info.lossy_description());
                  }
                });
        server_binding_task_complete.Signal();
      });

  auto init_completer = std::make_unique<InitCompleter>(
      [softmac_bridge = softmac_bridge.get(), completer = std::move(completer)](
          zx_status_t status, wlansoftmac_handle_t* rust_handle) mutable {
        WLAN_LAMBDA_TRACE_DURATION("SoftmacBridge startup_rust_completer");
        softmac_bridge->rust_handle_ = rust_handle;
        (*completer)(status);
      });

  async::PostTask(
      softmac_bridge->rust_dispatcher_.async_dispatcher(),
      [init_completer = std::move(init_completer),
       sta_shutdown_handler = std::move(sta_shutdown_handler),
       wlansoftmac_rust_ops = wlansoftmac_rust_ops,
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
            [](void* ctx, zx_status_t status, wlansoftmac_handle_t* rust_handle) {
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
              (*init_completer)(status, rust_handle);
              delete init_completer;
            },
            wlansoftmac_rust_ops, frame_sender, rust_buffer_provider, softmac_bridge_client_end));
      });

  // Wait for the tasks posted to softmac_bridge_server_dispatcher to complete before returning.
  // Otherwise, the softmac_bridge pointer captured by the task might not be valid when the task
  // runs.
  server_binding_task_complete.Wait();
  return fit::success(std::move(softmac_bridge));
}

zx_status_t SoftmacBridge::Stop(std::unique_ptr<StopCompleter> stop_completer) {
  WLAN_TRACE_DURATION();
  if (rust_handle_ == nullptr) {
    lerror("Failed to call stop_bridged_wlansoftmac()! Encountered NULL rust_handle_");
    return ZX_ERR_BAD_STATE;
  }
  stop_bridged_wlansoftmac(
      stop_completer.release(),
      [](void* ctx) {
        WLAN_LAMBDA_TRACE_DURATION("run StopCompleter");
        // Safety: `stop_completer` is now owned by this function, so it's safe to cast it to a
        // non-const pointer.
        auto stop_completer = static_cast<StopCompleter*>(ctx);
        if (stop_completer == nullptr) {
          lerror("Received NULL StopCompleter pointer!");
          return;
        }
        // Skip the check for whether stop_completer has already been called.  This is the only
        // location where stop_completer is called, and its deallocated immediately after. Thus,
        // such a check would be a use-after-free violation.
        (*stop_completer)();
        delete stop_completer;
      },
      rust_handle_);
  return ZX_OK;
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

  {
    auto status = ZX_OK;
    libsync::Completion request_returned;
    self->softmac_client_->QueueTx(fdf_request)
        .Then([&request_returned, &status, loc = cpp20::source_location::current(),
               async_id](fdf::Result<fuchsia_wlan_softmac::WlanSoftmac::QueueTx>& result) mutable {
          if (result.is_error()) {
            auto error = result.error_value();
            status = FidlErrorToStatus(error);
            WLAN_TRACE_ASYNC_END_TX(async_id, status);
          } else {
            WLAN_TRACE_ASYNC_END_TX(async_id, ZX_OK);
          }
          request_returned.Signal();
        });
    request_returned.Wait();
    if (status != ZX_OK) {
      return status;
    }
  }

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
