// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "network_device_shim.h"

#include <lib/async/cpp/task.h>
#include <lib/ddk/debug.h>
#include <lib/fdf/env.h>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

#include "log.h"

namespace network {

namespace netdev = fuchsia_hardware_network;

NetworkDeviceShim::NetworkDeviceShim(ddk::NetworkDeviceImplProtocolClient impl,
                                     const ShimDispatchers& dispatchers)
    : impl_(impl), dispatchers_(dispatchers) {}

zx::result<fdf::ClientEnd<netdriver::NetworkDeviceImpl>> NetworkDeviceShim::Bind() {
  auto endpoints = fdf::CreateEndpoints<netdriver::NetworkDeviceImpl>();
  if (endpoints.is_error()) {
    LOGF_ERROR("failed to create endpoints: %s", endpoints.status_string());
    return endpoints.take_error();
  }
  {
    fbl::AutoLock lock(&lock_);
    binding_ = fdf::BindServer(dispatchers_.shim_->get(), std::move(endpoints->server), this,
                               [this](NetworkDeviceShim*, fidl::UnbindInfo info,
                                      fdf::ServerEnd<netdriver::NetworkDeviceImpl>) {
                                 lock_.Acquire();
                                 binding_.reset();
                                 MaybeFinishTeardown();
                               });
  }
  return zx::ok(std::move(endpoints->client));
}

NetworkDeviceShim::~NetworkDeviceShim() {
  ZX_ASSERT_MSG(!binding_.has_value(), "NetworkDeviceShim destroyed before server unbound");
  ZX_ASSERT_MSG(network_port_shims_.is_empty(),
                "NetworkDeviceShim destroyed before all port shims destroyed");
}

NetworkDeviceImplBinder::Synchronicity NetworkDeviceShim::Teardown(
    fit::callback<void()>&& on_teardown_complete) {
  fbl::AutoLock lock(&lock_);
  if (binding_.has_value() || !network_port_shims_.is_empty()) {
    ZX_ASSERT(!on_teardown_complete_);
    // When Teardown is called all ports should already be removed or be in the process of being
    // removed. All that needs to be done is store the callback and unbind the server.
    on_teardown_complete_ = std::move(on_teardown_complete);
    if (binding_.has_value()) {
      // Make sure that unbinding happens asynchronously. Because the shim dispatchers are created
      // with an owner that's separate from the dispatcher that calls this method there is a chance
      // that the unbinding could get inlined if called directly. The FIDL part of the unbinding
      // will deadlock if that happens.
      async::PostTask(dispatchers_.shim_->async_dispatcher(),
                      [binding = std::move(binding_)]() mutable { binding->Unbind(); });
    }
    return Synchronicity::Async;
  }
  // Nothing to tear down, completed synchronously.
  return Synchronicity::Sync;
}

void NetworkDeviceShim::MaybeFinishTeardown() {
  // It's not safe to hold the lock during the call to on_teardown_complete_. The NetworkDeviceShim
  // could potentially be deleted in the callback which would mean that when the autolock destructs
  // and tries to unlock the mutex the mutex has already been destroyed. Instead move the callback
  // to a local variable and use that if available.
  fit::callback<void()> on_teardown_complete;
  if (on_teardown_complete_ && !binding_.has_value() && network_port_shims_.is_empty()) {
    // Everything is torn down, tear down is complete.
    on_teardown_complete = std::move(on_teardown_complete_);
  }
  lock_.Release();
  if (on_teardown_complete) {
    on_teardown_complete();
  }
}

void NetworkDeviceShim::Init(netdriver::wire::NetworkDeviceImplInitRequest* request,
                             fdf::Arena& arena, InitCompleter::Sync& completer) {
  device_ifc_ = fdf::WireSharedClient<netdriver::NetworkDeviceIfc>(std::move(request->iface),
                                                                   dispatchers_.port_->get());
  std::unique_ptr async_completer = std::make_unique<InitCompleter::Async>(completer.ToAsync());

  impl_.Init(
      this, &network_device_ifc_protocol_ops_,
      [](void* ctx, zx_status_t status) {
        std::unique_ptr<InitCompleter::Async> completer(static_cast<InitCompleter::Async*>(ctx));
        fdf::Arena arena('NETD');
        completer->buffer(arena).Reply(status);
      },
      async_completer.release());
}

void NetworkDeviceShim::Start(fdf::Arena& arena, StartCompleter::Sync& completer) {
  struct StartData {
    explicit StartData(StartCompleter::Sync& completer) : completer(completer.ToAsync()) {}

    StartCompleter::Async completer;
  };

  fbl::AllocChecker ac;
  std::unique_ptr cookie = fbl::make_unique_checked<StartData>(&ac, completer);
  if (!ac.check()) {
    LOGF_ERROR("no memory");
    completer.buffer(arena).Reply(ZX_ERR_NO_MEMORY);
    return;
  }

  impl_.Start(
      [](void* cookie, zx_status_t status) {
        std::unique_ptr<StartData> data(static_cast<StartData*>(cookie));
        fdf::Arena arena('NETD');
        data->completer.buffer(arena).Reply(status);
      },
      cookie.release());
}

void NetworkDeviceShim::Stop(fdf::Arena& arena, StopCompleter::Sync& completer) {
  struct StopData {
    explicit StopData(StopCompleter::Sync& completer) : completer(completer.ToAsync()) {}

    StopCompleter::Async completer;
  };

  fbl::AllocChecker ac;
  std::unique_ptr cookie = fbl::make_unique_checked<StopData>(&ac, completer);
  if (!ac.check()) {
    LOGF_ERROR("no memory");
    return;
  }

  impl_.Stop(
      [](void* cookie) {
        std::unique_ptr<StopData> data(static_cast<StopData*>(cookie));
        fdf::Arena arena('NETD');
        data->completer.buffer(arena).Reply();
      },
      cookie.release());
}

void NetworkDeviceShim::GetInfo(fdf::Arena& arena, GetInfoCompleter::Sync& completer) {
  device_impl_info_t info;
  impl_.GetInfo(&info);

  fidl::WireTableBuilder builder = netdriver::wire::DeviceImplInfo::Builder(arena);

  std::vector<netdev::wire::TxAcceleration> tx_accel;
  std::transform(info.tx_accel_list, info.tx_accel_list + info.tx_accel_count,
                 std::back_inserter(tx_accel),
                 [](const auto& accel) { return netdev::wire::TxAcceleration(accel); });
  fidl::VectorView tx_accel_view =
      fidl::VectorView<netdev::wire::TxAcceleration>::FromExternal(tx_accel);

  std::vector<netdev::wire::RxAcceleration> rx_accel;
  std::transform(info.rx_accel_list, info.rx_accel_list + info.tx_accel_count,
                 std::back_inserter(rx_accel),
                 [](const auto& accel) { return netdev::wire::RxAcceleration(accel); });
  fidl::VectorView rx_accel_view =
      fidl::VectorView<netdev::wire::RxAcceleration>::FromExternal(rx_accel);

  builder.device_features(info.device_features)
      .tx_depth(info.tx_depth)
      .rx_depth(info.rx_depth)
      .rx_threshold(info.rx_threshold)
      .max_buffer_parts(info.max_buffer_parts)
      .max_buffer_length(info.max_buffer_length)
      .buffer_alignment(info.buffer_alignment)
      .buffer_alignment(info.buffer_alignment)
      .min_rx_buffer_length(info.min_rx_buffer_length)
      .min_tx_buffer_length(info.min_tx_buffer_length)
      .tx_head_length(info.tx_head_length)
      .tx_tail_length(info.tx_tail_length)
      .tx_accel(fidl::ObjectView<decltype(tx_accel_view)>::FromExternal(&tx_accel_view))
      .rx_accel(fidl::ObjectView<decltype(rx_accel_view)>::FromExternal(&rx_accel_view));

  completer.buffer(arena).Reply(builder.Build());
}

void NetworkDeviceShim::QueueTx(netdriver::wire::NetworkDeviceImplQueueTxRequest* request,
                                fdf::Arena& arena, QueueTxCompleter::Sync& completer) {
  tx_buffer_t buffers[netdriver::kMaxTxBuffers];
  buffer_region_t regions[netdriver::kMaxTxBuffers * netdriver::kMaxBufferParts];
  size_t current_region = 0;

  for (size_t i = 0; i < request->buffers.count(); ++i) {
    const netdriver::wire::TxBuffer& fidl_buffer = request->buffers.at(i);
    const size_t num_regions = fidl_buffer.data.count();
    if (num_regions > netdriver::kMaxBufferParts) {
      LOGF_ERROR("Number of regions %zu exceeds maximum value of %u", num_regions,
                 netdriver::kMaxBufferParts);
      completer.Close(ZX_ERR_INVALID_ARGS);
      return;
    }
    for (size_t i = 0; i < num_regions; ++i) {
      const netdriver::wire::BufferRegion& fidl_region = fidl_buffer.data.at(i);
      regions[current_region + i] = {
          .vmo = fidl_region.vmo,
          .offset = fidl_region.offset,
          .length = fidl_region.length,
      };
    }
    buffers[i] = tx_buffer_t{
        .id = fidl_buffer.id,
        .data_list = regions + current_region,
        .data_count = num_regions,
        .meta =
            {
                .port = fidl_buffer.meta.port,
                .flags = fidl_buffer.meta.flags,
                .frame_type = static_cast<uint8_t>(fidl_buffer.meta.frame_type),
            },
        .head_length = fidl_buffer.head_length,
        .tail_length = fidl_buffer.tail_length,
    };
    current_region += num_regions;
  }

  impl_.QueueTx(buffers, request->buffers.count());
}

void NetworkDeviceShim::QueueRxSpace(netdriver::wire::NetworkDeviceImplQueueRxSpaceRequest* request,
                                     fdf::Arena& arena, QueueRxSpaceCompleter::Sync& completer) {
  rx_space_buffer_t buffers[netdriver::kMaxRxSpaceBuffers];
  for (size_t i = 0; i < request->buffers.count(); ++i) {
    const netdriver::wire::RxSpaceBuffer& fidl_buffer = request->buffers.at(i);
    buffers[i] = {
        .id = fidl_buffer.id,
        .region =
            {
                .vmo = fidl_buffer.region.vmo,
                .offset = fidl_buffer.region.offset,
                .length = fidl_buffer.region.length,
            },
    };
  }

  impl_.QueueRxSpace(buffers, request->buffers.count());
}

void NetworkDeviceShim::PrepareVmo(netdriver::wire::NetworkDeviceImplPrepareVmoRequest* request,
                                   fdf::Arena& arena, PrepareVmoCompleter::Sync& completer) {
  struct PrepareVmoData {
    explicit PrepareVmoData(PrepareVmoCompleter::Sync& completer)
        : completer(completer.ToAsync()) {}
    PrepareVmoCompleter::Async completer;
  };
  fbl::AllocChecker ac;
  std::unique_ptr cookie = fbl::make_unique_checked<PrepareVmoData>(&ac, completer);
  if (!ac.check()) {
    LOGF_ERROR("no memory");
    completer.buffer(arena).Reply(ZX_ERR_NO_MEMORY);
    return;
  }

  impl_.PrepareVmo(
      request->id, std::move(request->vmo),
      [](void* cookie, zx_status_t status) {
        std::unique_ptr<PrepareVmoData> data(static_cast<PrepareVmoData*>(cookie));
        fdf::Arena arena('NETD');
        data->completer.buffer(arena).Reply(status);
      },
      cookie.release());
}

void NetworkDeviceShim::ReleaseVmo(netdriver::wire::NetworkDeviceImplReleaseVmoRequest* request,
                                   fdf::Arena& arena, ReleaseVmoCompleter::Sync& completer) {
  impl_.ReleaseVmo(request->id);
  completer.buffer(arena).Reply();
}

void NetworkDeviceShim::NetworkDeviceIfcPortStatusChanged(uint8_t port_id,
                                                          const port_status_t* new_status) {
  fdf::Arena arena('NETD');
  fidl::WireTableBuilder builder = fuchsia_hardware_network::wire::PortStatus::Builder(arena);
  builder.mtu(new_status->mtu)
      .flags(fuchsia_hardware_network::wire::StatusFlags(new_status->flags));

  fidl::OneWayStatus status =
      device_ifc_.buffer(arena)->PortStatusChanged(port_id, builder.Build());
  if (!status.ok()) {
    LOGF_ERROR("PortStatusChanged error: %s", status.status_string());
  }
}

void NetworkDeviceShim::NetworkDeviceIfcAddPort(uint8_t port_id,
                                                const network_port_protocol_t* port,
                                                network_device_ifc_add_port_callback callback,
                                                void* cookie) {
  if (!device_ifc_.is_valid()) {
    LOGF_ERROR("invalid device interface, adding port before Init called?");
    callback(cookie, ZX_ERR_BAD_STATE);
    return;
  }
  if (dispatchers_.shim_->get() == nullptr) {
    LOGF_ERROR("missing dispatcher");
    callback(cookie, ZX_ERR_BAD_STATE);
    return;
  }

  // The NetworkPortShim constructor has to run on the same dispatcher that it's binding to. The
  // binding object in the port only allows access from that one dispatcher.
  async::PostTask(dispatchers_.port_->async_dispatcher(), [callback, cookie, port = *port, port_id,
                                                           this]() mutable {
    zx::result endpoints = fdf::CreateEndpoints<netdriver::NetworkPort>();
    if (endpoints.is_error()) {
      LOGF_ERROR("failed to create endpoints: %s", endpoints.status_string());
      callback(cookie, ZX_ERR_NO_RESOURCES);
      return;
    }

    ddk::NetworkPortProtocolClient impl(&port);
    std::unique_ptr port_shim = std::make_unique<NetworkPortShim>(
        impl, dispatchers_.port_->get(), std::move(endpoints->server),
        [this](NetworkPortShim* port_shim) {
          lock_.Acquire();
          network_port_shims_.erase(*port_shim);
          MaybeFinishTeardown();
        });

    fdf::Arena arena('NETD');
    device_ifc_.buffer(arena)
        ->AddPort(port_id, std::move(endpoints->client))
        .Then([callback, cookie, this, port_shim = std::move(port_shim)](auto& result) mutable {
          if (!result.ok()) {
            LOGF_ERROR("AddPort failed: %s", result.FormatDescription().c_str());
            callback(cookie, result.status());
          } else if (result.value().status != ZX_OK) {
            callback(cookie, result.value().status);
          } else {
            {
              fbl::AutoLock lock(&lock_);
              network_port_shims_.push_front(std::move(port_shim));
            }
            callback(cookie, ZX_OK);
          }
        });
  });
}

void NetworkDeviceShim::NetworkDeviceIfcRemovePort(uint8_t port_id) {
  fdf::Arena arena('NETD');
  fidl::OneWayStatus status = device_ifc_.buffer(arena)->RemovePort(port_id);
  if (!status.ok()) {
    LOGF_ERROR("RemovePort error: %s", status.status_string());
  }
}

void NetworkDeviceShim::NetworkDeviceIfcCompleteRx(const rx_buffer_t* rx_list, size_t rx_count) {
  constexpr size_t kCompleteRxRequestSize =
      fidl::MaxSizeInChannel<netdriver::wire::NetworkDeviceIfcCompleteRxRequest,
                             fidl::MessageDirection::kSending>();
  fidl::Arena<kCompleteRxRequestSize> arena;
  fidl::VectorView<netdriver::wire::RxBuffer> fidl_buffers(arena, rx_count);

  for (size_t i = 0; i < rx_count; ++i) {
    const rx_buffer_t& rx = rx_list[i];
    fidl::VectorView<netdriver::wire::RxBufferPart> buffer_parts(arena, rx.data_count);
    for (size_t j = 0; j < rx.data_count; ++j) {
      const rx_buffer_part_t& rx_part = rx.data_list[j];
      buffer_parts[j] = {
          .id = rx_part.id,
          .offset = rx_part.offset,
          .length = rx_part.length,
      };
    }

    fidl_buffers[i] = {
        .meta =
            {
                .port = rx.meta.port,
                .flags = rx.meta.flags,
                .frame_type = static_cast<fuchsia_hardware_network::FrameType>(rx.meta.frame_type),
            },
        .data = buffer_parts,
    };
  }

  fdf::Arena fdf_arena('NETD');
  auto status = device_ifc_.buffer(fdf_arena)->CompleteRx(fidl_buffers);
  if (!status.ok()) {
    LOGF_ERROR("CompleteRx error: %s", status.FormatDescription().c_str());
  }
}

void NetworkDeviceShim::NetworkDeviceIfcCompleteTx(const tx_result_t* tx_list, size_t tx_count) {
  constexpr size_t kCompleteTxRequestSize =
      fidl::MaxSizeInChannel<netdriver::wire::NetworkDeviceIfcCompleteTxRequest,
                             fidl::MessageDirection::kSending>();
  fidl::Arena<kCompleteTxRequestSize> arena;
  fidl::VectorView<netdriver::wire::TxResult> tx_results(arena, tx_count);

  for (size_t i = 0; i < tx_count; ++i) {
    const tx_result_t& tx = tx_list[i];
    tx_results[i] = {
        .id = tx.id,
        .status = tx.status,
    };
  }

  fdf::Arena fdf_arena('NETD');
  auto status = device_ifc_.buffer(fdf_arena)->CompleteTx(tx_results);

  if (!status.ok()) {
    LOGF_ERROR("CompleteTx error: %s", status.status_string());
  }
}

void NetworkDeviceShim::NetworkDeviceIfcDelegateRxLease(const delegated_rx_lease_t* delegated) {
  // NOTE: This conversion is a bit fraught and relies on the union being a
  // lease _channel_ because banjo bindings for unions do not carry a tag.
  //
  // This should, however, go away along with the last banjo users.
  constexpr size_t kDelegateRxLeaseRequestSize =
      fidl::MaxSizeInChannel<netdriver::wire::NetworkDeviceIfcDelegateRxLeaseRequest,
                             fidl::MessageDirection::kSending>();
  fidl::Arena<kDelegateRxLeaseRequestSize> arena;
  auto handle =
      netdev::wire::DelegatedRxLeaseHandle::WithChannel(zx::channel(delegated->handle.channel));

  uint64_t hold_until_frame = delegated->hold_until_frame;
  auto lease =
      netdev::wire::DelegatedRxLease::Builder(arena)
          .hold_until_frame(fidl::ObjectView<uint64_t>::FromExternal(&hold_until_frame))
          .handle(fidl::ObjectView<netdev::wire::DelegatedRxLeaseHandle>::FromExternal(&handle))
          .Build();

  fdf::Arena fdf_arena('NETD');
  auto status = device_ifc_.buffer(fdf_arena)->DelegateRxLease(lease);

  if (!status.ok()) {
    LOGF_ERROR("DelegateRxLease error: %s", status.status_string());
  }
}

}  // namespace network
