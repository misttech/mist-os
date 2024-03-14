// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "softmac_bridge.h"

#include <fidl/fuchsia.wlan.softmac/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fdf/dispatcher.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fit/function.h>
#include <lib/sync/cpp/completion.h>
#include <lib/trace/event.h>
#include <zircon/status.h>

#include <wlan/drivers/log.h>

#include "src/connectivity/wlan/drivers/wlansoftmac/rust_driver/c-binding/bindings.h"

namespace wlan::drivers::wlansoftmac {

SoftmacBridge::SoftmacBridge(
    DeviceInterface* device_interface,
    fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmac>&& softmac_client)
    : softmac_client_(
          std::forward<fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmac>>(softmac_client)),
      device_interface_(device_interface) {
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
    fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmac>&& softmac_client) {
  WLAN_TRACE_DURATION();
  auto softmac_bridge =
      std::unique_ptr<SoftmacBridge>(new SoftmacBridge(device, std::move(softmac_client)));

  // Safety: Each of the functions initialized in `wlansoftmac_rust_ops` is safe to call
  // from any thread as long they are never called concurrently.
  rust_device_interface_t wlansoftmac_rust_ops = {
      .device = static_cast<const void*>(softmac_bridge->device_interface_),
      .start = [](const void* device_interface, zx_handle_t softmac_ifc_bridge_client_handle,
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
      .deliver_eth_frame = [](const void* device_interface, const uint8_t* data,
                              size_t len) -> zx_status_t {
        WLAN_LAMBDA_TRACE_DURATION("rust_device_interface_t.deliver_eth_frame");
        return DeviceInterface::from(device_interface)->DeliverEthernet({data, len});
      },
      .queue_tx = [](const void* device_interface, uint32_t options, void* buffer, size_t written,
                     wlan_tx_info_t tx_info, trace_async_id_t async_id) -> zx_status_t {
        WLAN_LAMBDA_TRACE_DURATION("rust_device_interface_t.queue_tx");
        return DeviceInterface::from(device_interface)
            ->QueueTx(FinalizedBuffer::FromRaw(buffer, written), tx_info, async_id);
      },
      .set_ethernet_status = [](const void* device_interface, uint32_t status) -> zx_status_t {
        WLAN_LAMBDA_TRACE_DURATION("rust_device_interface_t.set_ethernet_status");
        return DeviceInterface::from(device_interface)->SetEthernetStatus(status);
      },
  };

  auto endpoints = fidl::CreateEndpoints<fuchsia_wlan_softmac::WlanSoftmacBridge>();
  if (endpoints.is_error()) {
    lerror("Failed to create WlanSoftmacBridge endpoints: %s", endpoints.status_string());
    return endpoints.take_error();
  }

  // Bind the WlanSoftmacBridge server on softmac_bridge_server_dispatcher. When the task
  // completes, it will signal server_binding_task_complete, primarily to indicate the captured
  // softmac_bridge pointer is no longer in use by the task.
  libsync::Completion server_binding_task_complete;
  async::PostTask(
      softmac_bridge_server_dispatcher.async_dispatcher(),
      [softmac_bridge = softmac_bridge.get(), server_endpoint = std::move(endpoints->server),
       &server_binding_task_complete]() mutable {
        WLAN_LAMBDA_TRACE_DURATION("WlanSoftmacBridge server binding");
        softmac_bridge->softmac_bridge_server_ =
            std::make_unique<fidl::ServerBinding<fuchsia_wlan_softmac::WlanSoftmacBridge>>(
                fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(server_endpoint),
                softmac_bridge, [](fidl::UnbindInfo info) {
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

  async::PostTask(softmac_bridge->rust_dispatcher_.async_dispatcher(),
                  [init_completer = std::move(init_completer),
                   sta_shutdown_handler = std::move(sta_shutdown_handler),
                   wlansoftmac_rust_ops = wlansoftmac_rust_ops,
                   rust_buffer_provider = softmac_bridge->rust_buffer_provider,
                   client_end = endpoints->client.TakeHandle().release()]() mutable {
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
                        wlansoftmac_rust_ops, rust_buffer_provider, client_end));
                  });

  // Wait for the task posted to softmac_bridge_server_dispatcher to complete before returning.
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
  Dispatcher<fuchsia_wlan_softmac::WlanSoftmac::Query> dispatcher = [](const auto& arena,
                                                                       const auto& softmac_client) {
    return softmac_client.sync().buffer(arena)->Query();
  };
  DispatchAndComplete(__func__, dispatcher, completer);
}

void SoftmacBridge::QueryDiscoverySupport(QueryDiscoverySupportCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  Dispatcher<fuchsia_wlan_softmac::WlanSoftmac::QueryDiscoverySupport> dispatcher =
      [](const auto& arena, const auto& client) {
        return client.sync().buffer(arena)->QueryDiscoverySupport();
      };
  DispatchAndComplete(__func__, dispatcher, completer);
}

void SoftmacBridge::QueryMacSublayerSupport(QueryMacSublayerSupportCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  Dispatcher<fuchsia_wlan_softmac::WlanSoftmac::QueryMacSublayerSupport> dispatcher =
      [](const auto& arena, const auto& client) {
        return client.sync().buffer(arena)->QueryMacSublayerSupport();
      };
  DispatchAndComplete(__func__, dispatcher, completer);
}

void SoftmacBridge::QuerySecuritySupport(QuerySecuritySupportCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  Dispatcher<fuchsia_wlan_softmac::WlanSoftmac::QuerySecuritySupport> dispatcher =
      [](const auto& arena, const auto& client) {
        return client.sync().buffer(arena)->QuerySecuritySupport();
      };
  DispatchAndComplete(__func__, dispatcher, completer);
}

void SoftmacBridge::QuerySpectrumManagementSupport(
    QuerySpectrumManagementSupportCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  Dispatcher<fuchsia_wlan_softmac::WlanSoftmac::QuerySpectrumManagementSupport> dispatcher =
      [](const auto& arena, const auto& client) {
        return client.sync().buffer(arena)->QuerySpectrumManagementSupport();
      };
  DispatchAndComplete(__func__, dispatcher, completer);
}

void SoftmacBridge::SetChannel(SetChannelRequestView request,
                               SetChannelCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  Dispatcher<fuchsia_wlan_softmac::WlanSoftmac::SetChannel> dispatcher =
      [request](const auto& arena, const auto& softmac_client) {
        return softmac_client.sync().buffer(arena)->SetChannel(*request);
      };
  DispatchAndComplete(__func__, dispatcher, completer);
}

void SoftmacBridge::JoinBss(JoinBssRequestView request, JoinBssCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  Dispatcher<fuchsia_wlan_softmac::WlanSoftmac::JoinBss> dispatcher =
      [request](const auto& arena, const auto& softmac_client) {
        return softmac_client.sync().buffer(arena)->JoinBss(request->join_request);
      };
  DispatchAndComplete(__func__, dispatcher, completer);
}

void SoftmacBridge::EnableBeaconing(EnableBeaconingRequestView request,
                                    EnableBeaconingCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  Dispatcher<fuchsia_wlan_softmac::WlanSoftmac::EnableBeaconing> dispatcher =
      [request](const auto& arena, const auto& softmac_client) {
        return softmac_client.sync().buffer(arena)->EnableBeaconing(*request);
      };
  DispatchAndComplete(__func__, dispatcher, completer);
}

void SoftmacBridge::DisableBeaconing(DisableBeaconingCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  Dispatcher<fuchsia_wlan_softmac::WlanSoftmac::DisableBeaconing> dispatcher =
      [](const auto& arena, const auto& softmac_client) {
        return softmac_client.sync().buffer(arena)->DisableBeaconing();
      };
  DispatchAndComplete(__func__, dispatcher, completer);
}

void SoftmacBridge::InstallKey(InstallKeyRequestView request,
                               InstallKeyCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  Dispatcher<fuchsia_wlan_softmac::WlanSoftmac::InstallKey> dispatcher =
      [request](const auto& arena, const auto& softmac_client) {
        return softmac_client.sync().buffer(arena)->InstallKey(*request);
      };
  DispatchAndComplete(__func__, dispatcher, completer);
}

void SoftmacBridge::NotifyAssociationComplete(NotifyAssociationCompleteRequestView request,
                                              NotifyAssociationCompleteCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  Dispatcher<fuchsia_wlan_softmac::WlanSoftmac::NotifyAssociationComplete> dispatcher =
      [request](const auto& arena, const auto& softmac_client) {
        return softmac_client.sync().buffer(arena)->NotifyAssociationComplete(request->assoc_cfg);
      };
  DispatchAndComplete(__func__, dispatcher, completer);
}

void SoftmacBridge::ClearAssociation(ClearAssociationRequestView request,
                                     ClearAssociationCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  Dispatcher<fuchsia_wlan_softmac::WlanSoftmac::ClearAssociation> dispatcher =
      [request](const auto& arena, const auto& softmac_client) {
        return softmac_client.sync().buffer(arena)->ClearAssociation(*request);
      };
  DispatchAndComplete(__func__, dispatcher, completer);
}

void SoftmacBridge::StartPassiveScan(StartPassiveScanRequestView request,
                                     StartPassiveScanCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  Dispatcher<fuchsia_wlan_softmac::WlanSoftmac::StartPassiveScan> dispatcher =
      [request](const auto& arena, const auto& softmac_client) {
        return softmac_client.sync().buffer(arena)->StartPassiveScan(*request);
      };
  DispatchAndComplete(__func__, dispatcher, completer);
}

void SoftmacBridge::StartActiveScan(StartActiveScanRequestView request,
                                    StartActiveScanCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  Dispatcher<fuchsia_wlan_softmac::WlanSoftmac::StartActiveScan> dispatcher =
      [request](const auto& arena, const auto& softmac_client) {
        return softmac_client.sync().buffer(arena)->StartActiveScan(*request);
      };
  DispatchAndComplete(__func__, dispatcher, completer);
}

void SoftmacBridge::CancelScan(CancelScanRequestView request,
                               CancelScanCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  Dispatcher<fuchsia_wlan_softmac::WlanSoftmac::CancelScan> dispatcher =
      [request](const auto& arena, const auto& softmac_client) {
        return softmac_client.sync().buffer(arena)->CancelScan(*request);
      };
  DispatchAndComplete(__func__, dispatcher, completer);
}

void SoftmacBridge::UpdateWmmParameters(UpdateWmmParametersRequestView request,
                                        UpdateWmmParametersCompleter::Sync& completer) {
  WLAN_TRACE_DURATION();
  Dispatcher<fuchsia_wlan_softmac::WlanSoftmac::UpdateWmmParameters> dispatcher =
      [request](const auto& arena, const auto& softmac_client) {
        return softmac_client.sync().buffer(arena)->UpdateWmmParameters(*request);
      };
  DispatchAndComplete(__func__, dispatcher, completer);
}

void SoftmacBridge::QueueEthFrameTx(eth::BorrowedOperation<> op, trace_async_id_t async_id) {
  WLAN_TRACE_DURATION();
  if (rust_handle_ == nullptr) {
    auto status = ZX_ERR_BAD_STATE;
    op.Complete(status);
    WLAN_TRACE_ASYNC_END_TX(async_id, status);
    return;
  }

  wlan_span_t span{.data = op.operation()->data_buffer, .size = op.operation()->data_size};
  op.Complete(sta_queue_eth_frame_tx(rust_handle_, span, async_id));
}

template <typename FidlMethod>
fidl::WireResultUnwrapType<FidlMethod> SoftmacBridge::FlattenAndLogError(
    const std::string& method_name, fdf::WireUnownedResult<FidlMethod> result) {
  WLAN_TRACE_DURATION();
  if (!result.ok()) {
    lerror("%s failed (FIDL error %s)", method_name.c_str(), result.status_string());
    return fit::error(result.status());
  }
  if (result->is_error()) {
    lerror("%s failed (status %s)", method_name.c_str(),
           zx_status_get_string(result->error_value()));
    return fit::error(result->error_value());
  }

  if constexpr (has_value_type<fidl::WireResultUnwrapType<FidlMethod>>) {
    return fit::success(std::move(result->value()));
  } else
    return fit::ok();
}

template <typename Completer, typename FidlMethod>
void SoftmacBridge::DispatchAndComplete(const std::string& method_name,
                                        Dispatcher<FidlMethod> dispatcher, Completer& completer) {
  WLAN_TRACE_DURATION();
  auto arena = fdf::Arena::Create(0, 0);
  if (arena.is_error()) {
    lerror("Arena creation failed: %s", arena.status_string());
    completer.ReplyError(ZX_ERR_INTERNAL);
    return;
  }

  auto result = FlattenAndLogError(method_name, dispatcher(*std::move(arena), softmac_client_));
  completer.Reply(std::move(result));
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
