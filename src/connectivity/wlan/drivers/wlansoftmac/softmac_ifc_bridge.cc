// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "softmac_ifc_bridge.h"

#include <fidl/fuchsia.wlan.softmac/cpp/driver/wire.h>
#include <fidl/fuchsia.wlan.softmac/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fidl/cpp/wire/client.h>
#include <lib/fidl_driver/cpp/transport.h>
#include <lib/sync/cpp/completion.h>
#include <lib/trace/event.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>

#include <wlan/drivers/log.h>

#include "src/connectivity/wlan/drivers/wlansoftmac/rust_driver/c-binding/bindings.h"

namespace wlan::drivers::wlansoftmac {

zx::result<std::unique_ptr<SoftmacIfcBridge>> SoftmacIfcBridge::New(
    const fdf::Dispatcher& softmac_ifc_server_dispatcher, const frame_processor_t* frame_processor,
    fdf::ServerEnd<fuchsia_wlan_softmac::WlanSoftmacIfc>&& server_endpoint,
    fidl::ClientEnd<fuchsia_wlan_softmac::WlanSoftmacIfcBridge>&&
        softmac_ifc_bridge_client_endpoint) {
  WLAN_TRACE_DURATION();
  auto softmac_ifc_bridge =
      std::unique_ptr<SoftmacIfcBridge>(new SoftmacIfcBridge(frame_processor));

  // Bind the WlanSoftmacIfc server and WlanSoftmacIfcBridge client on
  // softmac_ifc_bridge_server_dispatcher.
  libsync::Completion binding_task_complete;
  async::PostTask(
      softmac_ifc_server_dispatcher.async_dispatcher(),
      [softmac_ifc_bridge = softmac_ifc_bridge.get(), server_endpoint = std::move(server_endpoint),
       softmac_ifc_bridge_client_endpoint = std::move(softmac_ifc_bridge_client_endpoint),
       &binding_task_complete]() mutable {
        WLAN_LAMBDA_TRACE_DURATION("WlanSoftmacIfc server binding");
        softmac_ifc_bridge->softmac_ifc_server_binding_ =
            std::make_unique<fdf::ServerBinding<fuchsia_wlan_softmac::WlanSoftmacIfc>>(
                fdf::Dispatcher::GetCurrent()->get(), std::move(server_endpoint),
                softmac_ifc_bridge, [](fidl::UnbindInfo info) {
                  WLAN_LAMBDA_TRACE_DURATION("WlanSoftmacIfc close_handler");
                  if (info.is_user_initiated()) {
                    linfo("WlanSoftmacIfc server closed.");
                  } else {
                    lerror("WlanSoftmacIfc unexpectedly closed: %s", info.lossy_description());
                  }
                });
        softmac_ifc_bridge->softmac_ifc_bridge_client_ =
            std::make_unique<fidl::WireClient<fuchsia_wlan_softmac::WlanSoftmacIfcBridge>>(
                std::move(softmac_ifc_bridge_client_endpoint),
                fdf::Dispatcher::GetCurrent()->async_dispatcher());
        binding_task_complete.Signal();
      });
  binding_task_complete.Wait();

  return fit::ok(std::move(softmac_ifc_bridge));
}

void SoftmacIfcBridge::Recv(RecvRequestView fdf_request, fdf::Arena& fdf_arena,
                            RecvCompleter::Sync& completer) {
  trace_async_id_t async_id = TRACE_NONCE();
  WLAN_TRACE_ASYNC_BEGIN_RX(async_id);
  WLAN_TRACE_DURATION();

  fidl::Arena fidl_arena;
  auto builder = fuchsia_wlan_softmac::wire::FrameProcessorWlanRxRequest::Builder(fidl_arena);
  builder.packet_address(reinterpret_cast<uint64_t>(fdf_request->packet.mac_frame.begin()));
  builder.packet_size(reinterpret_cast<uint64_t>(fdf_request->packet.mac_frame.count()));
  builder.packet_info(fdf_request->packet.info);
  builder.async_id(async_id);
  auto fidl_request = builder.Build();

  auto fidl_request_persisted = ::fidl::Persist(fidl_request);
  if (fidl_request_persisted.is_ok()) {
    frame_processor_.ops->wlan_rx(frame_processor_.ctx, fidl_request_persisted.value().data(),
                                  fidl_request_persisted.value().size());
  } else {
    lerror("Failed to persist FrameProcessor.WlanRx fidl_request (FIDL error %s)",
           fidl_request_persisted.error_value());
  }
  completer.buffer(fdf_arena).Reply();
}

void SoftmacIfcBridge::ReportTxResult(ReportTxResultRequestView request, fdf::Arena& fdf_arena,
                                      ReportTxResultCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  auto result = softmac_ifc_bridge_client_->sync()->ReportTxResult(request->tx_result);
  if (!result.ok()) {
    lerror("ReportTxResult failed (FIDL error %s)", result.status_string());
  }
  completer.buffer(fdf_arena).Reply();
}

void SoftmacIfcBridge::NotifyScanComplete(NotifyScanCompleteRequestView request,
                                          fdf::Arena& fdf_arena,
                                          NotifyScanCompleteCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  auto result = softmac_ifc_bridge_client_->sync()->NotifyScanComplete(*request);
  if (!result.ok()) {
    lerror("NotifyScanComplete failed (FIDL error %s)", result.status_string());
  }
  completer.buffer(fdf_arena).Reply();
}

}  // namespace wlan::drivers::wlansoftmac
