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

#include <mutex>

#include "convert.h"
#include "src/connectivity/wlan/drivers/wlansoftmac/rust_driver/c-binding/bindings.h"

namespace wlan::drivers::wlansoftmac {

zx::result<std::unique_ptr<SoftmacIfcBridge>> SoftmacIfcBridge::New(
    const fdf::Dispatcher& softmac_ifc_server_dispatcher, std::shared_ptr<std::mutex> unbind_lock,
    std::shared_ptr<bool> unbind_called,
    const rust_wlan_softmac_ifc_protocol_copy_t* rust_softmac_ifc,
    fdf::ServerEnd<fuchsia_wlan_softmac::WlanSoftmacIfc>&& server_endpoint,
    fidl::ClientEnd<fuchsia_wlan_softmac::WlanSoftmacIfcBridge>&&
        softmac_ifc_bridge_client_endpoint) {
  WLAN_TRACE_DURATION();
  auto softmac_ifc_bridge = std::unique_ptr<SoftmacIfcBridge>(
      new SoftmacIfcBridge(std::move(unbind_lock), std::move(unbind_called)));

  // The protocol functions are stored in this class, which will act as
  // the server end of WlanSoftmacifc FIDL protocol, and this set of function pointers will be
  // called in the handler functions of FIDL server end.
  softmac_ifc_bridge->wlan_softmac_ifc_protocol_ops_ = {
      .recv = rust_softmac_ifc->ops->recv,
      .report_tx_result = rust_softmac_ifc->ops->report_tx_result,
      .notify_scan_complete = rust_softmac_ifc->ops->scan_complete,
  };
  softmac_ifc_bridge->wlan_softmac_ifc_protocol_.ops =
      &softmac_ifc_bridge->wlan_softmac_ifc_protocol_ops_;

  // The Banjo binding generates a `void*`. We use `const_cast` here to allow assignment but assert
  // the const-correctness in the Rust portion of this driver. In particular, this `ctx` is actually
  // a `*const DriverEventSink` in Rust. And all methods implementing the functions pointers
  // contained in `ops` immediately cast the `&mut DriveEventSink` passed to them to a
  // `&DriverEventSink`.
  softmac_ifc_bridge->wlan_softmac_ifc_protocol_.ctx = const_cast<void*>(rust_softmac_ifc->ctx);

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

void SoftmacIfcBridge::Recv(RecvRequestView request, fdf::Arena& arena,
                            RecvCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  {
    std::lock_guard<std::mutex> lock(*unbind_lock_);
    if (*unbind_called_) {
      return;
    }
  }

  wlan_rx_packet_t rx_packet;
  bool use_prealloc_recv_buffer =
      unlikely(request->packet.mac_frame.count() > kPreAllocRecvBufferSize);
  uint8_t* rx_packet_buffer;
  if (use_prealloc_recv_buffer) {
    rx_packet_buffer = static_cast<uint8_t*>(malloc(request->packet.mac_frame.count()));
  } else {
    rx_packet_buffer = pre_alloc_recv_buffer_;
  }

  zx_status_t status = ConvertRxPacket(request->packet, &rx_packet, rx_packet_buffer);
  if (status != ZX_OK) {
    lerror("RxPacket conversion failed: %s", zx_status_get_string(status));
  }

  wlan_softmac_ifc_protocol_.ops->recv(wlan_softmac_ifc_protocol_.ctx, &rx_packet);
  if (use_prealloc_recv_buffer) {
    // Freeing the frame buffer allocated in ConvertRxPacket() above.
    memset(const_cast<uint8_t*>(rx_packet.mac_frame_buffer), 0, rx_packet.mac_frame_size);
    free(const_cast<uint8_t*>(rx_packet.mac_frame_buffer));
  } else {
    memset(pre_alloc_recv_buffer_, 0, kPreAllocRecvBufferSize);
  }

  completer.buffer(arena).Reply();
}

void SoftmacIfcBridge::ReportTxResult(ReportTxResultRequestView request, fdf::Arena& arena,
                                      ReportTxResultCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  {
    std::lock_guard<std::mutex> lock(*unbind_lock_);
    if (*unbind_called_) {
      return;
    }
  }

  wlan_tx_result_t tx_result;
  zx_status_t status = ConvertTxStatus(request->tx_result, &tx_result);
  if (status != ZX_OK) {
    lerror("TxStatus conversion failed: %s", zx_status_get_string(status));
  }

  wlan_softmac_ifc_protocol_.ops->report_tx_result(wlan_softmac_ifc_protocol_.ctx, &tx_result);

  completer.buffer(arena).Reply();
}
void SoftmacIfcBridge::NotifyScanComplete(NotifyScanCompleteRequestView request, fdf::Arena& arena,
                                          NotifyScanCompleteCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  {
    std::lock_guard<std::mutex> lock(*unbind_lock_);
    if (*unbind_called_) {
      return;
    }
  }

  wlan_softmac_ifc_protocol_.ops->notify_scan_complete(wlan_softmac_ifc_protocol_.ctx,
                                                       request->status(), request->scan_id());
  completer.buffer(arena).Reply();
}

}  // namespace wlan::drivers::wlansoftmac
