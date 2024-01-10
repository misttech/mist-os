// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "softmac_bridge.h"

#include <fidl/fuchsia.wlan.softmac/cpp/fidl.h>
#include <lib/async/cpp/task.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fit/function.h>
#include <lib/sync/cpp/completion.h>

#include <wlan/drivers/log.h>

#include "src/connectivity/wlan/drivers/wlansoftmac/rust_driver/c-binding/bindings.h"

namespace wlan::drivers::wlansoftmac {

zx::result<std::unique_ptr<SoftmacBridge>> SoftmacBridge::New(
    fdf::Dispatcher& softmac_bridge_server_dispatcher, std::unique_ptr<StartStaCompleter> completer,
    DeviceInterface* device,
    fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmac>&& softmac_client) {
  WLAN_TRACE_DURATION();
  auto softmac_bridge = std::unique_ptr<SoftmacBridge>(new SoftmacBridge(
      device,
      std::forward<fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmac>>(softmac_client)));

  rust_device_interface_t wlansoftmac_rust_ops = {
      .device = static_cast<void*>(softmac_bridge->device_interface_),
      .start = [](void* device_interface, const rust_wlan_softmac_ifc_protocol_copy_t* ifc,
                  zx_handle_t* out_sme_channel) -> zx_status_t {
        zx::channel channel;
        zx_status_t result = AsDeviceInterface(device_interface)->Start(ifc, &channel);
        *out_sme_channel = channel.release();
        return result;
      },
      .deliver_eth_frame = [](void* device_interface, const uint8_t* data,
                              size_t len) -> zx_status_t {
        return AsDeviceInterface(device_interface)->DeliverEthernet({data, len});
      },
      .queue_tx = [](void* device_interface, uint32_t options, wlansoftmac_out_buf_t buf,
                     wlan_tx_info_t tx_info) -> zx_status_t {
        return AsDeviceInterface(device_interface)->QueueTx(UsedBuffer::FromOutBuf(buf), tx_info);
      },
      .set_ethernet_status = [](void* device_interface, uint32_t status) -> zx_status_t {
        return AsDeviceInterface(device_interface)->SetEthernetStatus(status);
      },
  };

  auto endpoints = fidl::CreateEndpoints<fuchsia_wlan_softmac::WlanSoftmacBridge>();
  if (endpoints.is_error()) {
    lerror("Failed to create WlanSoftmacBridge endpoints: %s", endpoints.status_string());
    return endpoints.take_error();
  }

  // Bind the WlanSoftmacBridge server on softmac_bridge_server_dispatcher. When the task completes,
  // it will signal server_binding_task_complete, primarily to indicate the captured softmac_bridge
  // pointer is no longer in use by the task.
  libsync::Completion server_binding_task_complete;
  async::PostTask(
      softmac_bridge_server_dispatcher.async_dispatcher(),
      [softmac_bridge = softmac_bridge.get(), server_endpoint = std::move(endpoints->server),
       &server_binding_task_complete]() mutable {
        softmac_bridge->softmac_bridge_server_ =
            std::make_unique<fidl::ServerBinding<fuchsia_wlan_softmac::WlanSoftmacBridge>>(
                fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(server_endpoint),
                softmac_bridge, [](fidl::UnbindInfo info) {
                  if (info.is_user_initiated()) {
                    linfo("WlanSoftmacBridge server closed.");
                  } else {
                    lerror("WlanSoftmacBridge unexpectedly closed: %s", info.lossy_description());
                  }
                });
        server_binding_task_complete.Signal();
      });

  softmac_bridge->rust_handle_ = start_sta(
      completer.release(),
      [](void* ctx, zx_status_t status) {
        auto completer = static_cast<StartStaCompleter*>(ctx);
        if (completer == nullptr) {
          lerror("Received NULL StartStaCompleter pointer!");
          return;
        }
        // Skip the check for whether completer has already been
        // called.  This is the only location where completer is
        // called, and its deallocated immediately after. Thus, such a
        // check would be a use-after-free violation.
        (*completer)(status);
        delete completer;
      },
      wlansoftmac_rust_ops, softmac_bridge->rust_buffer_provider,
      endpoints->client.TakeHandle().release());

  // Wait for the task posted to softmac_bridge_server_dispatcher to complete before returning.
  // Otherwise, the softmac_bridge pointer captured by the task might not be valid when the task
  // runs.
  server_binding_task_complete.Wait();
  return fit::success(std::move(softmac_bridge));
}

zx_status_t SoftmacBridge::StopSta(std::unique_ptr<StopStaCompleter> completer) {
  WLAN_TRACE_DURATION();
  if (rust_handle_ == nullptr) {
    lerror("Failed to call stop_sta()! Encountered NULL rust_handle_");
    return ZX_ERR_BAD_STATE;
  }
  stop_sta(
      completer.release(),
      [](void* ctx) {
        auto completer = static_cast<StopStaCompleter*>(ctx);
        if (completer == nullptr) {
          lerror("Received NULL StopStaCompleter pointer!");
          return;
        }
        // Skip the check for whether completer has already been
        // called.  This is the only location where completer is
        // called, and its deallocated immediately after. Thus, such a
        // check would be a use-after-free violation.
        (*completer)();
        delete completer;
      },
      rust_handle_);
  return ZX_OK;
}

SoftmacBridge::~SoftmacBridge() {
  WLAN_TRACE_DURATION();
  ldebug(0, nullptr, "Entering.");
  if (rust_handle_ == nullptr) {
    lerror("Failed to call delete_sta()! Encountered NULL rust_handle_");
  }
  delete_sta(rust_handle_);
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

void SoftmacBridge::QueueEthFrameTx(eth::BorrowedOperation<> op) {
  WLAN_TRACE_DURATION();
  if (rust_handle_ == nullptr) {
    op.Complete(ZX_ERR_BAD_STATE);
    return;
  }

  wlan_span_t span{.data = op.operation()->data_buffer, .size = op.operation()->data_size};
  op.Complete(sta_queue_eth_frame_tx(rust_handle_, span));
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

wlansoftmac_in_buf_t SoftmacBridge::IntoRustInBuf(std::unique_ptr<Buffer> owned_buffer) {
  WLAN_TRACE_DURATION();
  auto* buffer = owned_buffer.release();
  return wlansoftmac_in_buf_t{
      .free_buffer = [](void* raw) { std::unique_ptr<Buffer>(static_cast<Buffer*>(raw)).reset(); },
      .raw = buffer,
      .data = buffer->data(),
      .len = buffer->capacity(),
  };
}

}  // namespace wlan::drivers::wlansoftmac
