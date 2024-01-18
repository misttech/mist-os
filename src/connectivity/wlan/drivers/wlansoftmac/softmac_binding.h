// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_SOFTMAC_BINDING_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_SOFTMAC_BINDING_H_

#include <fidl/fuchsia.wlan.softmac/cpp/driver/wire.h>
#include <fuchsia/hardware/ethernet/cpp/banjo.h>
#include <fuchsia/wlan/common/c/banjo.h>
#include <fuchsia/wlan/internal/c/banjo.h>
#include <fuchsia/wlan/softmac/c/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/fdf/cpp/channel_read.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/operation/ethernet.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>
#include <zircon/compiler.h>

#include <memory>
#include <mutex>

#include <ddktl/device.h>
#include <fbl/ref_ptr.h>
#include <wlan/common/macaddr.h>

#include "buffer_allocator.h"
#include "device_interface.h"
#include "softmac_bridge.h"
#include "softmac_ifc_bridge.h"
#include "src/connectivity/wlan/drivers/wlansoftmac/rust_driver/c-binding/bindings.h"

namespace wlan::drivers::wlansoftmac {

class SoftmacBinding : public DeviceInterface {
 public:
  static zx::result<std::unique_ptr<SoftmacBinding>> New(
      zx_device_t* device, fdf::UnownedDispatcher&& main_driver_dispatcher);
  ~SoftmacBinding() override = default;

  static constexpr inline SoftmacBinding* from(void* ctx) {
    return static_cast<SoftmacBinding*>(ctx);
  }

  // DeviceInterface methods
  zx_status_t Start(const rust_wlan_softmac_ifc_protocol_copy_t* ifc,
                    zx_handle_t softmac_ifc_bridge_client_handle,
                    zx::channel* out_sme_channel) final;
  zx_status_t DeliverEthernet(cpp20::span<const uint8_t> eth_frame) final
      __TA_EXCLUDES(ethernet_proxy_lock_);
  zx_status_t QueueTx(UsedBuffer used_buffer, wlan_tx_info_t tx_info) final;
  zx_status_t SetEthernetStatus(uint32_t status) final __TA_EXCLUDES(ethernet_proxy_lock_);

 private:
  // Private constructor to require use of New().
  explicit SoftmacBinding(zx_device_t* device, fdf::UnownedDispatcher&& main_driver_dispatcher);
  zx_device_t* device_ = nullptr;

  /////////////////////////////////////
  // Member variables and methods to implement a child device
  // supporting the ZX_PROTOCOL_ETHERNET_IMPL custom protocol.
  zx_device_t* child_device_ = nullptr;
  fdf::UnownedDispatcher main_driver_dispatcher_;
  void Init();
  void Unbind();
  void Release();

  zx_status_t EthernetImplQuery(uint32_t options, ethernet_info_t* info);
  zx_status_t EthernetImplStart(const ethernet_ifc_protocol_t* ifc)
      __TA_EXCLUDES(ethernet_proxy_lock_);
  void EthernetImplStop() __TA_EXCLUDES(ethernet_proxy_lock_);
  void EthernetImplQueueTx(uint32_t options, ethernet_netbuf_t* netbuf,
                           ethernet_impl_queue_tx_callback callback, void* cookie);
  static zx_status_t EthernetImplSetParam(uint32_t param, int32_t value, const uint8_t* data_buffer,
                                          size_t data_size);
  static void EthernetImplGetBti(zx_handle_t* out_bti);

  const zx_protocol_device_t eth_device_ops_ = {
      .version = DEVICE_OPS_VERSION,
      .init = [](void* ctx) { SoftmacBinding::from(ctx)->Init(); },
      .unbind = [](void* ctx) { SoftmacBinding::from(ctx)->Unbind(); },
      .release = [](void* ctx) { SoftmacBinding::from(ctx)->Release(); },
  };

  const ethernet_impl_protocol_ops_t ethernet_impl_ops_ = {
      .query = [](void* ctx, uint32_t options, ethernet_info_t* info) -> zx_status_t {
        return SoftmacBinding::from(ctx)->EthernetImplQuery(options, info);
      },
      .stop = [](void* ctx) { SoftmacBinding::from(ctx)->EthernetImplStop(); },
      .start = [](void* ctx, const ethernet_ifc_protocol_t* ifc) -> zx_status_t {
        return SoftmacBinding::from(ctx)->EthernetImplStart(ifc);
      },
      .queue_tx =
          [](void* ctx, uint32_t options, ethernet_netbuf_t* netbuf,
             ethernet_impl_queue_tx_callback callback, void* cookie) {
            SoftmacBinding::from(ctx)->EthernetImplQueueTx(options, netbuf, callback, cookie);
          },
      .set_param = [](void* ctx, uint32_t param, int32_t value, const uint8_t* data_buffer,
                      size_t data_size) -> zx_status_t {
        return SoftmacBinding::EthernetImplSetParam(param, value, data_buffer, data_size);
      },
      .get_bti =
          [](void* ctx, zx_handle_t* out_bti) {
            SoftmacBinding::from(ctx)->EthernetImplGetBti(out_bti);
          },
  };

  std::mutex ethernet_proxy_lock_;
  ddk::EthernetIfcProtocolClient ethernet_proxy_ __TA_GUARDED(ethernet_proxy_lock_);

  // Manages the lifetime of the protocol struct we pass down to the vendor driver. Actual
  // calls to this protocol should only be performed by the vendor driver.
  std::unique_ptr<wlan_softmac_ifc_protocol_ops_t> wlan_softmac_ifc_protocol_ops_;
  std::unique_ptr<wlan_softmac_ifc_protocol_t> wlan_softmac_ifc_protocol_;

  fdf::Dispatcher softmac_bridge_server_dispatcher_;
  std::unique_ptr<SoftmacBridge> softmac_bridge_;

  // The FIDL client to communicate with iwlwifi
  fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmac> client_;

  fdf::Dispatcher softmac_ifc_server_dispatcher_;
  fdf::Dispatcher softmac_ifc_bridge_client_dispatcher_;
  std::unique_ptr<SoftmacIfcBridge> softmac_ifc_bridge_;

  // Record when the framework calls the unbind hook to prevent sta_shutdown_handler() from calling
  // device_async_remove() when an unbind is already in progress.
  //
  // The bool is behind a std::shared_ptr so sta_shutdown_handler() can reference
  // unbind_called_ even if SoftmacBinding drops its reference to unbind_called_.
  std::shared_ptr<bool> unbind_called_;

  // Dispatcher for being a FIDL client firing requests on WlanSoftmac protocol.
  fdf::Dispatcher client_dispatcher_;
};

}  // namespace wlan::drivers::wlansoftmac

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_SOFTMAC_BINDING_H_
