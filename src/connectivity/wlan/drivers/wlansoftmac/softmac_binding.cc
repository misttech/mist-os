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
#include <lib/fidl/cpp/wire/channel.h>
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
#include <wlan/drivers/log.h>

namespace wlan::drivers::wlansoftmac {

SoftmacBinding::SoftmacBinding()
    : unbind_lock_(std::make_shared<std::mutex>()), unbind_called_(std::make_shared<bool>(false)) {
  WLAN_TRACE_DURATION();
  ldebug(0, nullptr, "Entering.");
  linfo("Creating a new WLAN device.");

  ethernet_proxy_lock_ = std::make_shared<std::mutex>();

  // Create a dispatcher to serve the WlanSoftmacBridge protocol.
  {
    auto dispatcher = fdf::SynchronizedDispatcher::Create(
        fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "wlansoftmacbridge_server",
        [](fdf_dispatcher_t*) {
          WLAN_LAMBDA_TRACE_DURATION("wlansoftmacbridge_server shutdown_handler");
        });

    if (dispatcher.is_error()) {
      ZX_ASSERT_MSG(false, "Creating server dispatcher error: %s",
                    zx_status_get_string(dispatcher.status_value()));
    }

    softmac_bridge_server_dispatcher_ = *std::move(dispatcher);
  }

  // Create a dispatcher to serve the WlanSoftmacIfc protocol.
  {
    auto dispatcher = fdf::SynchronizedDispatcher::Create(
        fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "wlansoftmacifc_server",
        [](fdf_dispatcher_t*) {
          WLAN_LAMBDA_TRACE_DURATION("wlansoftmacifc_server shutdown_handler");
        });

    if (dispatcher.is_error()) {
      ZX_ASSERT_MSG(false, "Creating server dispatcher error: %s",
                    zx_status_get_string(dispatcher.status_value()));
    }

    softmac_ifc_server_dispatcher_ = *std::move(dispatcher);
  }

  // Create a dispatcher for WlanSoftmac method calls to the parent device.
  //
  // The Unbind hook relies on client_dispatcher_ implementing a shutdown
  // handler that performs the following steps in sequence.
  //
  //   - Asynchronously destroy softmac_ifc_bridge_
  //   - Asynchronously call device_unbind_reply()
  //
  // Each step of the sequence must occur on its respective dispatcher
  // to allow all queued task to complete.
  {
    auto dispatcher = fdf::SynchronizedDispatcher::Create(
        fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "wlansoftmac_client",
        [&](fdf_dispatcher_t* client_dispatcher) {
          WLAN_LAMBDA_TRACE_DURATION("wlansoftmac_client shutdown_handler");
          // Every fidl::ServerBinding must be destroyed on the
          // dispatcher its bound too.
          async::PostTask(softmac_ifc_server_dispatcher_.async_dispatcher(), [&]() {
            WLAN_LAMBDA_TRACE_DURATION("softmac_ifc_bridge reset + device_unbind_reply");
            softmac_ifc_bridge_.reset();
            device_unbind_reply(device_);
          });
          // Explicitly call destroy since Unbind() calls releases this dispatcher before
          // calling ShutdownAsync().
          fdf_dispatcher_destroy(client_dispatcher);
        });

    if (dispatcher.is_error()) {
      ZX_ASSERT_MSG(false, "Creating client dispatcher error: %s",
                    zx_status_get_string(dispatcher.status_value()));
    }
    client_dispatcher_ = *std::move(dispatcher);
  }
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
  client_ = fdf::WireSharedClient(std::move(endpoints->client), client_dispatcher_.get());
  linfo("Connected to WlanSoftmac service.");

  linfo("Starting up Rust WlanSoftmac...");
  auto completer = std::make_unique<fit::callback<void(zx_status_t status)>>(
      [main_device_dispatcher = main_device_dispatcher_->async_dispatcher(),
       device = device_](zx_status_t status) {
        WLAN_LAMBDA_TRACE_DURATION("startup_rust_completer");
        if (status == ZX_OK) {
          linfo("Completed Rust WlanSoftmac startup.");
        } else {
          lerror("Failed to startup Rust WlanSoftmac: %s", zx_status_get_string(status));
        }

        // device_init_reply() must be called on a driver framework managed
        // dispatcher
        async::PostTask(main_device_dispatcher, [device, status]() {
          WLAN_LAMBDA_TRACE_DURATION("device_init_reply");
          // Specify empty device_init_reply_args_t since SoftmacBinding
          // does not currently support power or performance state
          // information.
          device_init_reply(device, status, nullptr);
        });
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
    auto softmac_bridge = SoftmacBridge::New(
        softmac_bridge_server_dispatcher_, std::move(completer), std::move(sta_shutdown_handler),
        this, client_.Clone(), ethernet_proxy_lock_, &ethernet_proxy_);
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
  auto stop_returned = std::make_unique<libsync::Completion>();
  auto unowned_stop_returned = stop_returned.get();
  auto stop_completer = std::make_unique<StopCompleter>(
      [softmac_bridge_server_dispatcher = softmac_bridge_server_dispatcher_.async_dispatcher(),
       softmac_bridge, client_dispatcher = client_dispatcher_.release(),
       stop_returned = std::move(stop_returned)]() mutable {
        WLAN_LAMBDA_TRACE_DURATION("StopCompleter");
        async::PostTask(
            softmac_bridge_server_dispatcher,
            [softmac_bridge, client_dispatcher, stop_returned = std::move(stop_returned)]() {
              WLAN_LAMBDA_TRACE_DURATION("SoftmacBridge destruction");
              stop_returned->Wait();
              delete softmac_bridge;
              fdf_dispatcher_shutdown_async(client_dispatcher);
            });
      });
  softmac_bridge->Stop(std::move(stop_completer));
  unowned_stop_returned->Signal();
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
  if (info == nullptr)
    return ZX_ERR_INVALID_ARGS;

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return ZX_ERR_INTERNAL;
  }

  auto query_result = client_.sync().buffer(*std::move(arena))->Query();
  if (!query_result.ok()) {
    lerror("Failed getting query result (FIDL error %s)", query_result.status_string());
    return query_result.status();
  }
  if (query_result->is_error()) {
    lerror("Failed getting query result (status %s)",
           zx_status_get_string(query_result->error_value()));
    return query_result->error_value();
  }

  memset(info, 0, sizeof(*info));
  common::MacAddr(query_result->value()->sta_addr().data()).CopyTo(info->mac);
  info->features = ETHERNET_FEATURE_WLAN;

  auto query_mac_sublayer_result =
      client_.sync().buffer(*std::move(arena))->QueryMacSublayerSupport();
  if (!query_mac_sublayer_result.ok()) {
    lerror("Failed getting mac sublayer result (FIDL error %s)",
           query_mac_sublayer_result.status_string());
    return query_mac_sublayer_result.status();
  }
  if (query_mac_sublayer_result->value()->resp.device.is_synthetic) {
    info->features |= ETHERNET_FEATURE_SYNTH;
  }

  info->mtu = 1500;
  info->netbuf_size = eth::BorrowedOperation<>::OperationSize(sizeof(ethernet_netbuf_t));

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

  // Post a task to `softmac_ifc_server_dispatcher_` to sequence queuing the Ethernet frame
  // with other calls from `softmac_ifc_bridge_` to the bridged wlansoftmac driver. The
  // `SoftmacIfcBridge` class is not designed to be thread-safe. Making calls to its methods
  // from different dispatchers could result in unexpected behavior.
  async::PostTask(softmac_ifc_server_dispatcher_.async_dispatcher(),
                  [&, op = std::move(op), async_id]() {
                    auto result = softmac_ifc_bridge_->EthernetTx(op.get(), async_id);
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

zx_status_t SoftmacBinding::Start(zx_handle_t softmac_ifc_bridge_client_handle,
                                  const frame_processor_t* frame_processor,
                                  zx::channel* out_sme_channel) const {
  WLAN_TRACE_DURATION();
  debugf("Start");

  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    return ZX_ERR_INTERNAL;
  }

  auto endpoints = fdf::CreateEndpoints<fuchsia_wlan_softmac::WlanSoftmacIfc>();
  if (endpoints.is_error()) {
    lerror("Creating end point error: %s", endpoints.status_string());
    return endpoints.status_value();
  }

  zx::channel softmac_ifc_bridge_client_channel(softmac_ifc_bridge_client_handle);
  fidl::ClientEnd<fuchsia_wlan_softmac::WlanSoftmacIfcBridge> softmac_ifc_bridge_client_endpoint(
      std::move(softmac_ifc_bridge_client_channel));

  unbind_lock_->lock();
  auto softmac_ifc_bridge = SoftmacIfcBridge::New(softmac_ifc_server_dispatcher_, frame_processor,
                                                  std::move(endpoints->server),
                                                  std::move(softmac_ifc_bridge_client_endpoint));
  unbind_lock_->unlock();

  if (softmac_ifc_bridge.is_error()) {
    lerror("Failed to create SoftmacIfcBridge: %s", softmac_ifc_bridge.status_string());
    return softmac_ifc_bridge.status_value();
  }
  softmac_ifc_bridge_ = *std::move(softmac_ifc_bridge);

  auto start_response =
      client_.sync().buffer(*std::move(arena))->Start(std::move(endpoints->client));
  if (!start_response.ok()) {
    lerror("change channel failed (FIDL error %s)", start_response.status_string());
    return start_response.status();
  }
  if (start_response->is_error()) {
    lerror("change channel failed (status %s)",
           zx_status_get_string(start_response->error_value()));
    return start_response->error_value();
  }
  *out_sme_channel = std::move(start_response->value()->sme_channel);

  return ZX_OK;
}

zx_status_t SoftmacBinding::SetEthernetStatus(uint32_t status) const {
  WLAN_TRACE_DURATION();
  std::lock_guard<std::mutex> lock(*ethernet_proxy_lock_);
  if (ethernet_proxy_.is_valid()) {
    ethernet_proxy_.Status(status);
  }
  return ZX_OK;
}

}  // namespace wlan::drivers::wlansoftmac
