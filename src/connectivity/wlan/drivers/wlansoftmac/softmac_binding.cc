// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "softmac_binding.h"

#include <fidl/fuchsia.wlan.softmac/cpp/driver/fidl.h>
#include <fidl/fuchsia.wlan.softmac/cpp/fidl.h>
#include <fuchsia/hardware/ethernet/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/cpp/task.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fdf/dispatcher.h>
#include <lib/fit/result.h>
#include <lib/operation/ethernet.h>
#include <lib/sync/cpp/completion.h>
#include <lib/trace/event.h>
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
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <memory>
#include <mutex>
#include <utility>

#include <fbl/ref_ptr.h>
#include <wlan/common/channel.h>
#include <wlan/drivers/fidl_bridge.h>
#include <wlan/drivers/log.h>

namespace wlan::drivers::wlansoftmac {

using ::wlan::drivers::fidl_bridge::FidlErrorToStatus;

SoftmacBinding::SoftmacBinding()
    : ethernet_proxy_lock_(std::make_shared<std::mutex>()),
      unbind_lock_(std::make_shared<std::mutex>()),
      unbind_called_(std::make_shared<bool>(false)) {
  WLAN_TRACE_DURATION();
  linfo("Creating a new WLAN device.");
}

// Disable thread safety analysis, as this is a part of device initialization.
// All thread-unsafe work should occur before multiple threads are possible
// (e.g., before MainLoop is started and before DdkAdd() is called), or locks
// should be held.
zx::result<std::unique_ptr<SoftmacBinding>> SoftmacBinding::New(zx_device_t* parent_device)
    __TA_NO_THREAD_SAFETY_ANALYSIS {
  WLAN_TRACE_DURATION();
  ldebug(0, nullptr, "Entering.");
  linfo("Binding...");
  auto softmac_binding = std::unique_ptr<SoftmacBinding>(new SoftmacBinding());

  device_add_args_t args = {
      .version = DEVICE_ADD_ARGS_VERSION,
      .name = "wlansoftmac-ethernet",
      .ctx = softmac_binding.get(),
      .ops = &softmac_binding->eth_device_ops_,
      .proto_id = ZX_PROTOCOL_ETHERNET_IMPL,
      .proto_ops = &softmac_binding->ethernet_impl_ops_,
  };
  auto status = device_add(parent_device, &args, &softmac_binding->device_);
  if (status != ZX_OK) {
    lerror("could not add eth device: %s", zx_status_get_string(status));
    return fit::error(status);
  }

  return fit::success(std::move(softmac_binding));
}

// ddk ethernet_impl_protocol_ops methods

void SoftmacBinding::Init() {
  WLAN_TRACE_DURATION();
  ldebug(0, nullptr, "Entering.");
  linfo("Initializing...");
  main_device_dispatcher_ = fdf::Dispatcher::GetCurrent();

  auto endpoints = fdf::CreateEndpoints<fuchsia_wlan_softmac::Service::WlanSoftmac::ProtocolType>();
  if (endpoints.is_error()) {
    lerror("Failed to create FDF endpoints: %s", endpoints.status_string());
    device_init_reply(device_, endpoints.status_value(), nullptr);
    return;
  }

  auto status = device_connect_runtime_protocol(
      device_, fuchsia_wlan_softmac::Service::WlanSoftmac::ServiceName,
      fuchsia_wlan_softmac::Service::WlanSoftmac::Name, endpoints->server.TakeChannel().release());
  if (status != ZX_OK) {
    lerror("Failed to connect to WlanSoftmac service: %s", zx_status_get_string(status));
    device_init_reply(device_, status, nullptr);
    return;
  }
  client_ = fdf::SharedClient(std::move(endpoints->client), main_device_dispatcher_->get());
  linfo("Connected to WlanSoftmac service.");

  linfo("Starting up Rust WlanSoftmac...");
  auto completer = std::make_unique<fit::callback<void(zx_status_t status)>>(
      [device = device_](zx_status_t status) {
        WLAN_LAMBDA_TRACE_DURATION("startup_rust_completer + device_init_reply");
        if (status == ZX_OK) {
          linfo("Completed Rust WlanSoftmac startup.");
        } else {
          lerror("Failed to startup Rust WlanSoftmac: %s", zx_status_get_string(status));
        }
        // Specify empty device_init_reply_args_t since SoftmacBinding
        // does not currently support power or performance state
        // information.
        device_init_reply(device, status, nullptr);
      });

  unbind_lock_->lock();
  fit::callback<void(zx_status_t)> sta_shutdown_handler = [unbind_lock = unbind_lock_,
                                                           unbind_called = unbind_called_,
                                                           device = device_](zx_status_t status) {
    WLAN_LAMBDA_TRACE_DURATION("sta_shutdown_handler on Rust dispatcher");
    if (status == ZX_OK) {
      return;
    }
    lerror("Rust thread had an abnormal shutdown: %s", zx_status_get_string(status));
    std::lock_guard<std::mutex> lock(*unbind_lock);
    if (*unbind_called) {
      linfo("Skipping device_async_remove() since Release() already called.");
      return;
    }
    device_async_remove(device);
  };
  unbind_lock_->unlock();

  {
    std::lock_guard<std::mutex> lock(*ethernet_proxy_lock_);
    auto softmac_bridge =
        SoftmacBridge::New(std::move(completer), std::move(sta_shutdown_handler), client_.Clone(),
                           ethernet_proxy_lock_, &ethernet_proxy_, &cached_ethernet_status_);
    if (softmac_bridge.is_error()) {
      lerror("Failed to create SoftmacBridge: %s", softmac_bridge.status_string());
      device_init_reply(device_, softmac_bridge.error_value(), nullptr);
      return;
    }
    softmac_bridge_ = std::move(*softmac_bridge);
  }
}

// See lib/ddk/device.h for documentation on when this method is called.
void SoftmacBinding::Unbind() {
  WLAN_TRACE_DURATION();
  std::lock_guard<std::mutex> lock(*unbind_lock_);
  *unbind_called_ = true;

  ldebug(0, nullptr, "Entering.");
  auto softmac_bridge = softmac_bridge_.release();

  // Note that SoftmacBridge::Stop will return before StopCompleter is called because
  // the SoftmacIfcBridge.softmac_ifc_bridge_client_ runs on the same dispatcher as
  // this function (SoftmacBinding::Unbind).
  auto stop_completer =
      std::make_unique<fit::callback<void()>>([softmac_bridge, device = device_]() mutable {
        WLAN_LAMBDA_TRACE_DURATION("SoftmacBridge destruction + device_unbind_reply");
        delete softmac_bridge;
        device_unbind_reply(device);
      });
  softmac_bridge->StopBridgedDriver(std::move(stop_completer));
}

// See lib/ddk/device.h for documentation on when this method is called.
void SoftmacBinding::Release() {
  WLAN_TRACE_DURATION();
  ldebug(0, nullptr, "Entering.");
  delete this;
}

zx_status_t SoftmacBinding::EthernetImplQuery(uint32_t options, ethernet_info_t* info) {
  WLAN_TRACE_DURATION();
  ldebug(0, nullptr, "Entering.");
  if (info == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }

  *info = {
      .features = ETHERNET_FEATURE_WLAN,
      .mtu = 1500,
      .netbuf_size = eth::BorrowedOperation<>::OperationSize(sizeof(ethernet_netbuf_t)),
  };

  auto result = [&, info]() -> fit::result<zx_status_t> {
    zx_status_t status = ZX_OK;
    {
      // Use a libsync::Completion to make this call synchronous since
      // SoftmacBinding::EthernetImplQuery does not provide a completer.
      //
      // This synchronous call is a potential risk for deadlock in the ethernet device. Deadlock is
      // unlikely to occur because the third-party driver is unlikely to rely on a response from the
      // ethernet device to respond to this request.
      //
      // Note: This method is called from an ethernet device dispatcher because this method is
      // implemented with a Banjo binding.
      libsync::Completion request_returned;
      client_->Query().Then(
          [&request_returned, &status,
           info](fdf::Result<fuchsia_wlan_softmac::WlanSoftmac::Query>& result) mutable {
            if (result.is_error()) {
              auto error = result.error_value();
              lerror("Failed getting query result (FIDL error %s)", error);
              status = FidlErrorToStatus(error);
            } else {
              common::MacAddr(result.value().sta_addr()->data()).CopyTo(info->mac);
            }
            request_returned.Signal();
          });
      request_returned.Wait();
    }
    if (status != ZX_OK) {
      return fit::error(status);
    }

    {
      // Use a libsync::Completion to make this call synchronous since
      // SoftmacBinding::EthernetImplQuery does not provide a completer.
      //
      // This synchronous call is a potential risk for deadlock in the ethernet device. Deadlock is
      // unlikely to occur because the third-party driver is unlikely to rely on a response from the
      // ethernet device to respond to this request.
      //
      // Note: This method is called from an ethernet device dispatcher because this method is
      // implemented with a Banjo binding.
      libsync::Completion request_returned;
      client_->QueryMacSublayerSupport().Then(
          [&request_returned, &status,
           info](fdf::Result<fuchsia_wlan_softmac::WlanSoftmac::QueryMacSublayerSupport>&
                     result) mutable {
            if (result.is_error()) {
              auto error = result.error_value();
              lerror("Failed getting mac sublayer result (FIDL error %s)", error);
              status = FidlErrorToStatus(error);
            } else {
              if (result.value().resp().device().is_synthetic()) {
                info->features |= ETHERNET_FEATURE_SYNTH;
              }
            }
            request_returned.Signal();
          });
      request_returned.Wait();
    }
    if (status != ZX_OK) {
      return fit::error(status);
    }

    return fit::ok();
  }();

  if (result.is_error()) {
    *info = {};
    return result.error_value();
  }
  return ZX_OK;
}

zx_status_t SoftmacBinding::EthernetImplStart(const ethernet_ifc_protocol_t* ifc) {
  WLAN_TRACE_DURATION();
  ldebug(0, nullptr, "Entering.");
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

void SoftmacBinding::EthernetImplStop() {
  WLAN_TRACE_DURATION();
  ldebug(0, nullptr, "Entering.");

  std::lock_guard<std::mutex> lock(*ethernet_proxy_lock_);
  if (!ethernet_proxy_.is_valid()) {
    lwarn("ethmac not started");
  }
  ethernet_proxy_.clear();
}

void SoftmacBinding::EthernetImplQueueTx(uint32_t options, ethernet_netbuf_t* netbuf,
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
  async::PostTask(main_device_dispatcher_->async_dispatcher(), [&, op = std::move(op), async_id]() {
    auto result = softmac_bridge_->EthernetTx(op.get(), async_id);
    if (!result.is_ok()) {
      WLAN_TRACE_ASYNC_END_TX(async_id, result.status_value());
    }
    op->Complete(result.status_value());
  });
}

zx_status_t SoftmacBinding::EthernetImplSetParam(uint32_t param, int32_t value,
                                                 const uint8_t* data_buffer, size_t data_size) {
  WLAN_TRACE_DURATION();
  ldebug(0, nullptr, "Entering.");
  if (param == ETHERNET_SETPARAM_PROMISC) {
    // See https://fxbug.dev/42103570: In short, the bridge mode doesn't require WLAN
    // promiscuous mode enabled.
    //               So we give a warning and return OK here to continue the
    //               bridging.
    // TODO(https://fxbug.dev/42103829): To implement the real promiscuous mode.
    if (value == 1) {  // Only warn when enabling.
      lwarn("WLAN promiscuous not supported yet. see https://fxbug.dev/42103829");
    }
    return ZX_OK;
  }
  return ZX_ERR_NOT_SUPPORTED;
}

void SoftmacBinding::EthernetImplGetBti(zx_handle_t* out_bti) {
  WLAN_TRACE_DURATION();
  lerror("WLAN does not support ETHERNET_FEATURE_DMA");
}

}  // namespace wlan::drivers::wlansoftmac
