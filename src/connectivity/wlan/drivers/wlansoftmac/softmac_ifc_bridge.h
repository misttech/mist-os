// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_SOFTMAC_IFC_BRIDGE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_SOFTMAC_IFC_BRIDGE_H_

#include <fidl/fuchsia.wlan.softmac/cpp/driver/wire.h>
#include <fidl/fuchsia.wlan.softmac/cpp/wire.h>
#include <fuchsia/wlan/softmac/c/banjo.h>
#include <lib/fidl/cpp/wire/client.h>
#include <lib/fidl_driver/cpp/transport.h>
#include <lib/zx/result.h>

#include <wlan/drivers/log.h>

#include "src/connectivity/wlan/drivers/wlansoftmac/rust_driver/c-binding/bindings.h"

namespace wlan::drivers::wlansoftmac {

class SoftmacIfcBridge : public fdf::WireServer<fuchsia_wlan_softmac::WlanSoftmacIfc> {
 public:
  static zx::result<std::unique_ptr<SoftmacIfcBridge>> New(
      fdf::Dispatcher& softmac_ifc_server_dispatcher,
      const rust_wlan_softmac_ifc_protocol_copy_t* rust_softmac_ifc,
      fdf::ServerEnd<fuchsia_wlan_softmac::WlanSoftmacIfc>&& server_endpoint);
  ~SoftmacIfcBridge() override = default;

  void Recv(RecvRequestView request, fdf::Arena& arena, RecvCompleter::Sync& completer) override;
  void ReportTxResult(ReportTxResultRequestView request, fdf::Arena& arena,
                      ReportTxResultCompleter::Sync& completer) override;
  void NotifyScanComplete(NotifyScanCompleteRequestView request, fdf::Arena& arena,
                          NotifyScanCompleteCompleter::Sync& completer) override;

 private:
  SoftmacIfcBridge() = default;

  wlan_softmac_ifc_protocol_t wlan_softmac_ifc_protocol_;
  wlan_softmac_ifc_protocol_ops_t wlan_softmac_ifc_protocol_ops_;
  std::unique_ptr<fdf::ServerBinding<fuchsia_wlan_softmac::WlanSoftmacIfc>>
      softmac_ifc_server_binding_;

  // Preallocated buffer for small frames
  static const size_t kPreAllocRecvBufferSize = 2000;
  uint8_t pre_alloc_recv_buffer_[kPreAllocRecvBufferSize];
};

}  // namespace wlan::drivers::wlansoftmac

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_SOFTMAC_IFC_BRIDGE_H_
