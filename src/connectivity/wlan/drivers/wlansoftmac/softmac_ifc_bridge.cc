// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "softmac_ifc_bridge.h"

#include <fidl/fuchsia.wlan.softmac/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fidl_driver/cpp/transport.h>
#include <lib/operation/ethernet.h>
#include <lib/sync/cpp/completion.h>
#include <lib/trace/event.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>

#include <wlan/drivers/fidl_bridge.h>
#include <wlan/drivers/log.h>

#include "src/connectivity/wlan/drivers/wlansoftmac/rust_driver/c-binding/bindings.h"

namespace wlan::drivers::wlansoftmac {

using ::wlan::drivers::fidl_bridge::ForwardResult;

zx::result<std::unique_ptr<SoftmacIfcBridge>> SoftmacIfcBridge::New(
    const ethernet_tx_t* ethernet_tx, const wlan_rx_t* wlan_rx,
    fdf::ServerEnd<fuchsia_wlan_softmac::WlanSoftmacIfc>&& server_endpoint,
    fidl::ClientEnd<fuchsia_wlan_softmac::WlanSoftmacIfcBridge>&&
        softmac_ifc_bridge_client_endpoint) {
  WLAN_TRACE_DURATION();
  auto softmac_ifc_bridge =
      std::unique_ptr<SoftmacIfcBridge>(new SoftmacIfcBridge(ethernet_tx, wlan_rx));

  {
    WLAN_LAMBDA_TRACE_DURATION("WlanSoftmacIfc server binding");
    softmac_ifc_bridge->softmac_ifc_server_binding_ =
        std::make_unique<fdf::ServerBinding<fuchsia_wlan_softmac::WlanSoftmacIfc>>(
            fdf::Dispatcher::GetCurrent()->get(), std::move(server_endpoint),
            softmac_ifc_bridge.get(), [](fidl::UnbindInfo info) {
              WLAN_LAMBDA_TRACE_DURATION("WlanSoftmacIfc close_handler");
              if (info.is_user_initiated()) {
                linfo("WlanSoftmacIfc server closed.");
              } else {
                lerror("WlanSoftmacIfc unexpectedly closed: %s", info.lossy_description());
              }
            });
  }

  softmac_ifc_bridge->softmac_ifc_bridge_client_ =
      std::make_unique<fidl::Client<fuchsia_wlan_softmac::WlanSoftmacIfcBridge>>(
          std::move(softmac_ifc_bridge_client_endpoint),
          fdf::Dispatcher::GetCurrent()->async_dispatcher());

  return fit::ok(std::move(softmac_ifc_bridge));
}

void SoftmacIfcBridge::Recv(RecvRequest& fdf_request, RecvCompleter::Sync& completer) {
  trace_async_id_t async_id = TRACE_NONCE();
  WLAN_TRACE_ASYNC_BEGIN_RX(async_id);
  WLAN_TRACE_DURATION();

  fuchsia_wlan_softmac::WlanRxTransferRequest fidl_request;
  fidl_request.packet_address(reinterpret_cast<uint64_t>(fdf_request.packet().mac_frame().data()));
  fidl_request.packet_size(reinterpret_cast<uint64_t>(fdf_request.packet().mac_frame().size()));
  fidl_request.packet_info(fdf_request.packet().info());
  fidl_request.async_id(async_id);

  auto fidl_request_persisted = ::fidl::Persist(fidl_request);
  if (fidl_request_persisted.is_ok()) {
    wlan_rx_.transfer(wlan_rx_.ctx, fidl_request_persisted.value().data(),
                      fidl_request_persisted.value().size());
  } else {
    lerror("Failed to persist WlanRx.Tranfer fidl_request (FIDL error %s)",
           fidl_request_persisted.error_value());
  }
  completer.Reply();
}

zx::result<> SoftmacIfcBridge::EthernetTx(eth::BorrowedOperation<>* op,
                                          trace_async_id_t async_id) const {
  WLAN_TRACE_DURATION();
  fuchsia_wlan_softmac::EthernetTxTransferRequest request;
  request.packet_address(reinterpret_cast<uint64_t>(op->operation()->data_buffer));
  request.packet_size(reinterpret_cast<uint64_t>(op->operation()->data_size));
  request.async_id(async_id);

  auto fidl_request_persisted = ::fidl::Persist(request);
  if (!fidl_request_persisted.is_ok()) {
    lerror("Failed to persist EthernetTx.Transfer request (FIDL error %s)",
           fidl_request_persisted.error_value());
  }

  auto result =
      zx::make_result(ethernet_tx_.transfer(ethernet_tx_.ctx, fidl_request_persisted.value().data(),
                                            fidl_request_persisted.value().size()));
  return result;
}

void SoftmacIfcBridge::ReportTxResult(ReportTxResultRequest& request,
                                      ReportTxResultCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  (*softmac_ifc_bridge_client_)
      ->ReportTxResult(request)
      .Then(ForwardResult<fuchsia_wlan_softmac::WlanSoftmacIfcBridge::ReportTxResult>(
          completer.ToAsync()));
}

void SoftmacIfcBridge::NotifyScanComplete(NotifyScanCompleteRequest& request,
                                          NotifyScanCompleteCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  (*softmac_ifc_bridge_client_)
      ->NotifyScanComplete(request)
      .Then(ForwardResult<fuchsia_wlan_softmac::WlanSoftmacIfcBridge::NotifyScanComplete>(
          completer.ToAsync()));
}

void SoftmacIfcBridge::StopBridgedDriver(std::unique_ptr<fit::callback<void()>> stop_completer) {
  WLAN_TRACE_DURATION();
  (*softmac_ifc_bridge_client_)
      ->StopBridgedDriver()
      .Then(
          [stop_completer = std::move(stop_completer)](
              fidl::Result<fuchsia_wlan_softmac::WlanSoftmacIfcBridge::StopBridgedDriver>& result) {
            (*stop_completer)();
          });
}

}  // namespace wlan::drivers::wlansoftmac
