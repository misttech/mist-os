// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "softmac_driver.h"

#include <fidl/fuchsia.wlan.softmac/cpp/driver/fidl.h>
#include <fidl/fuchsia.wlan.softmac/cpp/fidl.h>
#include <fuchsia/hardware/ethernet/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/cpp/task.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fdf/dispatcher.h>
#include <lib/fit/result.h>
#include <lib/operation/ethernet.h>
#include <lib/sync/cpp/completion.h>
#include <lib/trace-engine/types.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>
#include <lib/zx/thread.h>
#include <lib/zx/time.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/port.h>

#include <cstdarg>
#include <cstdio>
#include <cstring>
#include <memory>
#include <mutex>
#include <optional>
#include <utility>

#include <fbl/ref_ptr.h>
#include <wlan/common/channel.h>
#include <wlan/drivers/fidl_bridge.h>
#include <wlan/drivers/log.h>

namespace wlan::drivers::wlansoftmac {

using ::wlan::drivers::fidl_bridge::FidlErrorToStatus;

SoftmacDriver::SoftmacDriver(fdf::DriverStartArgs start_args,
                             fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : fdf::DriverBase("wlansoftmac", std::move(start_args), std::move(driver_dispatcher)),
      banjo_server_({ZX_PROTOCOL_ETHERNET_IMPL, this, &ethernet_impl_protocol_ops_}),
      ethernet_proxy_lock_(std::make_shared<std::mutex>()) {
  WLAN_TRACE_DURATION();
  FDF_LOG(INFO, "Creating a new WLAN device.");

  ethernet_impl_protocol_ops_ = {
      .query = [](void* ctx, uint32_t options, ethernet_info_t* info) -> zx_status_t {
        WLAN_LAMBDA_TRACE_DURATION("eth_impl_protocol_ops_t.query");
        return SoftmacDriver::from(ctx)->EthernetImplQuery(options, info);
      },
      .stop =
          [](void* ctx) {
            WLAN_LAMBDA_TRACE_DURATION("eth_impl_protocol_ops_t.stop");
            SoftmacDriver::from(ctx)->EthernetImplStop();
          },
      .start = [](void* ctx, const ethernet_ifc_protocol_t* ifc) -> zx_status_t {
        WLAN_LAMBDA_TRACE_DURATION("eth_impl_protocol_ops_t.start");
        return SoftmacDriver::from(ctx)->EthernetImplStart(ifc);
      },
      .queue_tx =
          [](void* ctx, uint32_t options, ethernet_netbuf_t* netbuf,
             ethernet_impl_queue_tx_callback callback, void* cookie) {
            WLAN_LAMBDA_TRACE_DURATION("eth_impl_protocol_ops_t.queue_tx");
            SoftmacDriver::from(ctx)->EthernetImplQueueTx(options, netbuf, callback, cookie);
          },
      .set_param = [](void* ctx, uint32_t param, int32_t value, const uint8_t* data_buffer,
                      size_t data_size) -> zx_status_t {
        WLAN_LAMBDA_TRACE_DURATION("eth_impl_protocol_ops_t.set_param");
        return SoftmacDriver::from(ctx)->EthernetImplSetParam(param, value, data_buffer, data_size);
      },
      .get_bti =
          [](void* ctx, zx_handle_t* out_bti2) {
            WLAN_LAMBDA_TRACE_DURATION("eth_impl_protocol_ops_t.get_bti");
            SoftmacDriver::from(ctx)->EthernetImplGetBti(reinterpret_cast<zx::bti*>(out_bti2));
          },
  };
}

void SoftmacDriver::Start(fdf::StartCompleter completer) {
  WLAN_TRACE_DURATION();
  FDF_LOG(INFO, "Starting wlansoftmac driver.");

  node_client_.Bind(std::move(node()), dispatcher());

  auto softmac_client = incoming()->Connect<fuchsia_wlan_softmac::Service::WlanSoftmac>();
  if (softmac_client.is_error()) {
    FDF_LOG(ERROR, "Failed to create FDF endpoints: %s", softmac_client.status_string());
    completer(softmac_client.take_error());
    return;
  }

  softmac_client_.Bind(*std::move(softmac_client), driver_dispatcher()->get());
  FDF_LOG(INFO, "Connected to WlanSoftmac service.");

  compat::DeviceServer::BanjoConfig banjo_config;
  banjo_config.callbacks[ZX_PROTOCOL_ETHERNET_IMPL] = banjo_server_.callback();

  compat_server_.Begin(
      incoming(), outgoing(), node_name(), "compat-server",
      [&, completer = std::move(completer)](zx::result<> compat_server_init_result) mutable {
        if (compat_server_init_result.is_error()) {
          FDF_LOG(ERROR, "Compat Server initialization failed: %s",
                  compat_server_init_result.status_string());
          completer(compat_server_init_result);
          return;
        }

        fuchsia_driver_framework::NodeAddChildRequest request;
        request.args({{
            .name = std::string("wlansoftmac-ethernet"),
            .properties = {{
                banjo_server_.property(),
            }},
            .offers2 = compat_server_.CreateOffers2(),
        }});

        auto controller_endpoints =
            fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();
        request.controller(std::move(controller_endpoints.server));

        node_client_->AddChild(std::move(request))
            .Then([&, completer = std::move(completer)](
                      fidl::Result<fuchsia_driver_framework::Node::AddChild> result) mutable {
              if (result.is_error()) {
                FDF_LOG(ERROR, "Failed to add ethernet device child: %s",
                        result.error_value().FormatDescription().c_str());
              }

              if (result.is_error()) {
                completer(compat_server_init_result);
                return;
              }

              FDF_LOG(INFO, "Successfully added ethernet device child.");

              auto start_completer = std::move(completer);
              fit::callback<void(zx_status_t)> shutdown_completer =
                  [node_client = node_client_.Clone()](zx_status_t status) mutable {
                    WLAN_LAMBDA_TRACE_DURATION("sta_shutdown_handler on Rust dispatcher");
                    if (status != ZX_OK) {
                      FDF_LOG(ERROR, "Bridged wlansoftmac driver had an abnormal shutdown: %s",
                              zx_status_get_string(status));

                      // Initiate asynchronous teardown of the fuchsia.driver.framework/Node proxy
                      // to cause the driver framework to stop this driver. Stopping this driver is
                      // appropriate when the bridge has an abnormal shutdown because otherwise this
                      // driver would be in an unusable state.
                      node_client.AsyncTeardown();
                      return;
                    }
                    FDF_LOG(INFO, "Bridged wlansoftmac driver shutdown complete.");
                  };

              {
                std::lock_guard<std::mutex> lock(*ethernet_proxy_lock_);
                auto softmac_bridge = SoftmacBridge::New(
                    node_client_.Clone(), std::move(start_completer), std::move(shutdown_completer),
                    softmac_client_.Clone(), ethernet_proxy_lock_, &ethernet_proxy_,
                    &cached_ethernet_status_);
                if (softmac_bridge.is_error()) {
                  FDF_LOG(ERROR, "Failed to create SoftmacBridge: %s",
                          softmac_bridge.status_string());
                  return;
                }
                softmac_bridge_ = std::move(*softmac_bridge);
              }
            });
      },
      compat::ForwardMetadata::None(), std::move(banjo_config));
}

void SoftmacDriver::PrepareStop(fdf::PrepareStopCompleter completer) {
  WLAN_TRACE_DURATION();
  auto softmac_bridge = softmac_bridge_.release();

  // Note that SoftmacBridge::StopBridgedDriver will return before the provided callback
  // because this function runs on the same dispatcher.
  softmac_bridge->StopBridgedDriver([softmac_bridge, completer = std::move(completer)]() mutable {
    WLAN_LAMBDA_TRACE_DURATION("SoftmacBridge destruction");

    // MLME acknowledges the DriverEvent::Stop message only after MLME
    // will no longer use the WlanSoftmacBridge client. As a result,
    // deleting the server here will not trigger any errors in MLME.
    delete softmac_bridge;
    completer(zx::ok());
  });
}

void SoftmacDriver::Stop() { WLAN_TRACE_DURATION(); }

zx_status_t SoftmacDriver::EthernetImplQuery(uint32_t options, ethernet_info_t* out_info) {
  WLAN_TRACE_DURATION();
  if (out_info == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }

  *out_info = {
      .features = ETHERNET_FEATURE_WLAN,
      .mtu = 1500,
      .netbuf_size = eth::BorrowedOperation<>::OperationSize(sizeof(ethernet_netbuf_t)),
  };

  auto cleanup = fit::defer([out_info]() { *out_info = {}; });

  zx_status_t status = ZX_OK;
  {
    // Use a libsync::Completion to make this call synchronous since
    // SoftmacDriver::EthernetImplQuery does not provide a completer.
    //
    // This synchronous call is a potential risk for deadlock in the ethernet device. Deadlock
    // is unlikely to occur because the third-party driver is unlikely to rely on a response
    // from the ethernet device to respond to this request.
    //
    // Note: This method is called from an ethernet device dispatcher because this method is
    // implemented with a Banjo binding.
    libsync::Completion request_returned;
    softmac_client_->Query().Then(
        [&request_returned, &status,
         out_info](fdf::Result<fuchsia_wlan_softmac::WlanSoftmac::Query>& result) mutable {
          if (result.is_error()) {
            status = FidlErrorToStatus(result.error_value());
            FDF_LOG(ERROR, "Failed getting query result (FIDL error %s)",
                    zx_status_get_string(status));
          } else {
            common::MacAddr(result.value().sta_addr()->data()).CopyTo(out_info->mac);
          }
          request_returned.Signal();
        });
    request_returned.Wait();
  }
  if (status != ZX_OK) {
    return status;
  }

  {
    // Use a libsync::Completion to make this call synchronous since
    // SoftmacDriver::EthernetImplQuery does not provide a completer.
    //
    // This synchronous call is a potential risk for deadlock in the ethernet device. Deadlock
    // is unlikely to occur because the third-party driver is unlikely to rely on a response
    // from the ethernet device to respond to this request.
    //
    // Note: This method is called from an ethernet device dispatcher because this method is
    // implemented with a Banjo binding.
    libsync::Completion request_returned;
    softmac_client_->QueryMacSublayerSupport().Then(
        [&request_returned, &status,
         out_info](fdf::Result<fuchsia_wlan_softmac::WlanSoftmac::QueryMacSublayerSupport>&
                       result) mutable {
          if (result.is_error()) {
            status = FidlErrorToStatus(result.error_value());
            FDF_LOG(ERROR, "Failed getting mac sublayer result (FIDL error %s)",
                    zx_status_get_string(status));
          } else {
            if (result.value().resp().device().is_synthetic()) {
              out_info->features |= ETHERNET_FEATURE_SYNTH;
            }
          }
          request_returned.Signal();
        });
    request_returned.Wait();
  }
  if (status != ZX_OK) {
    return status;
  }

  cleanup.cancel();
  return ZX_OK;
}

zx_status_t SoftmacDriver::EthernetImplStart(const ethernet_ifc_protocol_t* ifc) {
  WLAN_TRACE_DURATION();
  ZX_DEBUG_ASSERT(ifc != nullptr);

  std::lock_guard<std::mutex> lock(*ethernet_proxy_lock_);
  if (ethernet_proxy_.is_valid()) {
    return ZX_ERR_ALREADY_BOUND;
  }
  ethernet_proxy_ = ddk::EthernetIfcProtocolClient(ifc);

  // If MLME sets the ethernet status before the child device calls `EthernetImpl.Start`, then
  // the latest status will be in `cached_ethernet_status_`. If `cached_ethernet_status_` has
  // a status, then that status must be forwarded with `EthernetImplIfc.Status`.
  //
  // Otherwise, if the cached status is `ONLINE` and not forwarded, the child device will never
  // open its data path. The data path will then only open the next time MLME sets the status
  // to `ONLINE` which would be upon reassociation.
  if (cached_ethernet_status_.has_value()) {
    ethernet_proxy_.Status(*cached_ethernet_status_);
    cached_ethernet_status_.reset();
  }

  return ZX_OK;
}

void SoftmacDriver::EthernetImplStop() {
  WLAN_TRACE_DURATION();
  std::lock_guard<std::mutex> lock(*ethernet_proxy_lock_);
  if (!ethernet_proxy_.is_valid()) {
    FDF_LOG(WARNING, "EthernetImpl.Stop called when not started");
  }
  ethernet_proxy_.clear();
}

void SoftmacDriver::EthernetImplQueueTx(uint32_t options, ethernet_netbuf_t* netbuf,
                                        ethernet_impl_queue_tx_callback callback, void* cookie) {
  trace_async_id_t async_id = TRACE_NONCE();
  WLAN_TRACE_ASYNC_BEGIN_TX(async_id, "ethernet");
  WLAN_TRACE_DURATION();

  auto op = std::make_unique<eth::BorrowedOperation<>>(netbuf, callback, cookie,
                                                       sizeof(ethernet_netbuf_t));

  // Post a task to sequence queuing the Ethernet frame with other calls from
  // `softmac_ifc_bridge_` to the bridged wlansoftmac driver. The `SoftmacIfcBridge`
  // class is not designed to be thread-safe. Making calls to its methods from
  // different dispatchers could result in unexpected behavior.
  async::PostTask(dispatcher(), [&, op = std::move(op), async_id]() mutable {
    auto result = softmac_bridge_->EthernetTx(std::move(op), async_id);
    if (!result.is_ok()) {
      WLAN_TRACE_ASYNC_END_TX(async_id, result.status_value());
    }
  });
}

zx_status_t SoftmacDriver::EthernetImplSetParam(uint32_t param, int32_t value,
                                                const uint8_t* data_buffer, size_t data_size) {
  WLAN_TRACE_DURATION();
  if (param == ETHERNET_SETPARAM_PROMISC) {
    // See https://fxbug.dev/42103570: In short, the bridge mode doesn't require WLAN
    // promiscuous mode enabled.
    //               So we give a warning and return OK here to continue the
    //               bridging.
    // TODO(https://fxbug.dev/42103829): To implement the real promiscuous mode.
    if (value == 1) {  // Only warn when enabling.
      FDF_LOG(WARNING, "Promiscuous mode not supported. See https://fxbug.dev/42103829");
    }
    return ZX_OK;
  }
  return ZX_ERR_NOT_SUPPORTED;
}

void SoftmacDriver::EthernetImplGetBti(zx::bti* out_bti2) {
  WLAN_TRACE_DURATION();
  FDF_LOG(ERROR, "wlansoftmac does not support ETHERNET_FEATURE_DMA");
}

}  // namespace wlan::drivers::wlansoftmac

FUCHSIA_DRIVER_EXPORT(wlan::drivers::wlansoftmac::SoftmacDriver);
