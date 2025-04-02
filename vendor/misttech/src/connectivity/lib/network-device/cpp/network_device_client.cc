// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "network_device_client.h"

#include <lib/mistos/util/back_insert_iterator.h>
#include <lib/zx/time.h>
#include <trace.h>
#include <zircon/status.h>
#include <zircon/syscalls/port.h>

#include <algorithm>

#include <object/port_dispatcher.h>

#include "vendor/misttech/src/connectivity/network/drivers/network-device/device/device_port.h"

#define LOCAL_TRACE 0

namespace network {
namespace client {

namespace {
// The maximum FIFO depth that this client can handle.
// Set to the maximum number of `uint16`s that a zx FIFO can hold.
constexpr uint64_t kMaxDepth = ZX_PAGE_SIZE / sizeof(uint16_t);

constexpr zx_signals_t kFifoWaitReads = ZX_FIFO_READABLE | ZX_FIFO_PEER_CLOSED;
constexpr zx_signals_t kFifoWaitWrites = ZX_FIFO_WRITABLE;

static constexpr uint64_t kTxKey = 0;
static constexpr uint64_t kRxKey = 1;
static constexpr uint64_t kTxWritableKey = 2;
static constexpr uint64_t kRxWritableKey = 3;

}  // namespace

zx::result<DeviceInfo> DeviceInfo::Create(const device_info_t& device_info) {
  // TODO (Herrera) : Implment has_value() for device_info_t
#if 0
  if (!(fidl.has_min_descriptor_length() && fidl.has_descriptor_version() &&
        fidl.has_base_info())) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
#endif
  const device_base_info_t& base_info = device_info.base_info;

#if 0
   if (!(base_info.has_rx_depth() && base_info.has_tx_depth() && base_info.has_buffer_alignment() &&
        base_info.has_min_rx_buffer_length() && base_info.has_min_tx_buffer_length() &&
        base_info.has_min_tx_buffer_head() && base_info.has_min_tx_buffer_tail() &&
        base_info.has_max_buffer_parts())) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
#endif

  uint32_t max_buffer_length = std::numeric_limits<uint32_t>::max();
  if (base_info.max_buffer_length > 0) {
    max_buffer_length = base_info.max_buffer_length;
    if (max_buffer_length == 0) {
      return zx::error(ZX_ERR_INVALID_ARGS);
    }
  }

  DeviceInfo info = {
      .min_descriptor_length = device_info.min_descriptor_length,
      .descriptor_version = device_info.descriptor_version,
      .rx_depth = base_info.rx_depth,
      .tx_depth = base_info.tx_depth,
      .buffer_alignment = base_info.buffer_alignment,
      .max_buffer_length = max_buffer_length,
      .min_rx_buffer_length = base_info.min_rx_buffer_length,
      .min_tx_buffer_length = base_info.min_tx_buffer_length,
      .min_tx_buffer_head = base_info.min_tx_buffer_head,
      .min_tx_buffer_tail = base_info.min_tx_buffer_tail,
      .max_buffer_parts = base_info.max_buffer_parts,
  };

  if (base_info.rx_accel_count > 0) {
    cpp20::span<const uint8_t> rx_accel_span(&base_info.rx_accel_list[0], base_info.rx_accel_count);
    std::ranges::copy(rx_accel_span, util::back_inserter(info.rx_accel));
  }
  if (base_info.tx_accel_count > 0) {
    cpp20::span<const uint8_t> tx_accel_span(&base_info.tx_accel_list[0], base_info.tx_accel_count);
    std::ranges::copy(tx_accel_span, util::back_inserter(info.tx_accel));
  }

  return zx::ok(std::move(info));
}

zx::result<PortInfoAndMac> PortInfoAndMac::Create(
    const port_info_t& fidl, const std::optional<mac_address_t>& unicast_address) {
  if (!(fidl.id.base > 0 && fidl.base_info.port_class > 0)) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  const port_base_info_t& fidl_base_info = fidl.base_info;
  PortInfoAndMac info = {
      .id = fidl.id,
      .port_class = fidl_base_info.port_class,
      .unicast_address = unicast_address,
  };

  if (fidl_base_info.rx_types_count > 0) {
    auto& rx_types = fidl_base_info.rx_types_list;
    cpp20::span<const frame_type_t> rx_types_span(&rx_types[0], fidl_base_info.rx_types_count);
    std::ranges::copy(rx_types_span, util::back_inserter(info.rx_types));
  }
  if (fidl_base_info.tx_types_count > 0) {
    auto& tx_types = fidl_base_info.tx_types_list;
    cpp20::span<const frame_type_support_t> tx_types_span(&tx_types[0],
                                                          fidl_base_info.tx_types_count);
    std::ranges::copy(tx_types_span, util::back_inserter(info.tx_types));
  }

  return zx::ok(std::move(info));
}

NetworkDeviceClient::NetworkDeviceClient(
    std::unique_ptr<network::internal::DeviceInterface> device_interface)
    : device_(ktl::move(device_interface)) {
  KernelHandle<PortDispatcher> port;
  zx_rights_t rights;
  ZX_ASSERT(PortDispatcher::Create(0, &port, &rights) == ZX_OK);
  port_ = port.release();
}

#if 0
void NetworkDeviceClient::OnDeviceError(fidl::UnbindInfo info) {
  if (info.status() == ZX_ERR_PEER_CLOSED) {
    FX_LOGS(WARNING) << "device detached";
  } else {
    FX_LOGS(ERROR) << "device handler error: " << info;
  }
  ErrorTeardown(info.status());
}

void NetworkDeviceClient::OnSessionError(fidl::UnbindInfo info) {
  switch (info.status()) {
    case ZX_ERR_PEER_CLOSED:
    case ZX_ERR_CANCELED:
      FX_LOGS(WARNING) << "session detached: " << info;
      break;
    default:
      FX_LOGS(ERROR) << "session handler error: " << info;
      break;
  }
  ErrorTeardown(info.status());
}
#endif
NetworkDeviceClient::~NetworkDeviceClient() = default;

SessionConfig NetworkDeviceClient::DefaultSessionConfig(const DeviceInfo& dev_info) {
  const uint32_t buffer_length = std::min(kDefaultBufferLength, dev_info.max_buffer_length);
  // This allows us to align up without a conditional, as explained here:
  // https://stackoverflow.com/a/9194117
  const uint64_t buffer_stride =
      ((buffer_length + dev_info.buffer_alignment - 1) / dev_info.buffer_alignment) *
      dev_info.buffer_alignment;
  return {
      .buffer_length = buffer_length,
      .buffer_stride = buffer_stride,
      .descriptor_length = sizeof(buffer_descriptor_t),
      .tx_header_length = dev_info.min_tx_buffer_head,
      .tx_tail_length = dev_info.min_tx_buffer_tail,
      .rx_descriptor_count = dev_info.rx_depth,
      .tx_descriptor_count = dev_info.tx_depth,
      .options = SESSION_FLAGS_PRIMARY,
  };
}

void NetworkDeviceClient::OpenSession(const std::string& name,
                                      NetworkDeviceClient::OpenSessionCallback callback,
                                      NetworkDeviceClient::SessionConfigFactory config_factory) {
  if (session_running_) {
    callback(ZX_ERR_ALREADY_EXISTS);
    return;
  }
  session_running_ = true;

  zx::result<DeviceInfo> device_info = DeviceInfo::Create(device_->GetInfo());
  if (device_info.is_error()) {
    callback(device_info.error_value());
    return;
  }

  session_config_ = config_factory(device_info.value());
  device_info_ = std::move(device_info.value());
  zx_status_t status;
  if ((status = PrepareSession()) != ZX_OK) {
    callback(status);
    return;
  }

  zx::result session_info = MakeSessionInfo();
  if (session_info.is_error()) {
    callback(session_info.error_value());
    return;
  }

  zx::result open_result = device_->OpenSession(name, session_info.value());
  if (open_result.is_error()) {
    callback(open_result.error_value());
    return;
  }

  auto& [session, fifos] = open_result.value();
  session_ = session;
  ZX_ASSERT(session_);

  rx_fifo_ = ktl::move(fifos.first);
  tx_fifo_ = ktl::move(fifos.second);

  if ((status = PrepareDescriptors()) != ZX_OK) {
    callback(status);
    return;
  }

  thread_ =
      Thread::Create("netdevice:client", &NetworkDeviceClient::Thread, this, DEFAULT_PRIORITY);
  ZX_ASSERT_MSG(thread_, "Thread::Create failed.");
  thread_->Resume();

  callback(ZX_OK);
}

zx_status_t SessionConfig::Validate() {
  if (buffer_length <= tx_header_length + tx_tail_length) {
    LTRACEF("Invalid buffer length (%u), too small for requested Tx tail: (%u) + head: (%u)\n",
            buffer_length, tx_tail_length, tx_header_length);
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

zx_status_t NetworkDeviceClient::PrepareSession() {
  if (session_config_.descriptor_length < sizeof(buffer_descriptor_t) ||
      (session_config_.descriptor_length % sizeof(uint64_t)) != 0) {
    LTRACEF("Invalid descriptor length %lu\n", session_config_.descriptor_length);
    return ZX_ERR_INVALID_ARGS;
  }

  if (session_config_.rx_descriptor_count > kMaxDepth ||
      session_config_.tx_descriptor_count > kMaxDepth) {
    LTRACEF(
        "Invalid descriptor count %u/%u, this client supports a maximum depth of %lu descriptors\n",
        session_config_.rx_descriptor_count, session_config_.tx_descriptor_count, kMaxDepth);
    return ZX_ERR_INVALID_ARGS;
  }

  if (session_config_.buffer_stride < session_config_.buffer_length) {
    LTRACEF("Stride in VMO can't be smaller than buffer length\n");
    return ZX_ERR_INVALID_ARGS;
  }

  if (session_config_.buffer_stride % device_info_.buffer_alignment != 0) {
    LTRACEF("Buffer stride %lu does not meet buffer alignment requirement: %u\n",
            session_config_.buffer_stride, device_info_.buffer_alignment);
    return ZX_ERR_INVALID_ARGS;
  }

  descriptor_count_ = session_config_.rx_descriptor_count + session_config_.tx_descriptor_count;
  // Check if sum of descriptor count overflows.
  if (descriptor_count_ < session_config_.rx_descriptor_count ||
      descriptor_count_ < session_config_.tx_descriptor_count) {
    LTRACEF("Invalid descriptor count, maximum total descriptors must be less than 2^16\n");
    return ZX_ERR_INVALID_ARGS;
  }

  if (zx_status_t status = session_config_.Validate(); status != ZX_OK) {
    return status;
  }

  KernelHandle<VmObjectDispatcher> data_vmo;
  uint64_t data_vmo_size = descriptor_count_ * session_config_.buffer_stride;
  if (zx_status_t status =
          data_.CreateAndMap(data_vmo_size, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, nullptr, &data_vmo);
      status != ZX_OK) {
    LTRACEF("Failed to create data VMO: %s\n", zx_status_get_string(status));
    return status;
  }
  data_vmo_ = data_vmo.release();

  KernelHandle<VmObjectDispatcher> descriptors_vmo;
  uint64_t descriptors_vmo_size = descriptor_count_ * session_config_.descriptor_length;
  if (zx_status_t status = descriptors_.CreateAndMap(
          descriptors_vmo_size, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, nullptr, &descriptors_vmo);
      status != ZX_OK) {
    LTRACEF("Failed to create descriptors VMO: %s\n", zx_status_get_string(status));
    return status;
  }
  descriptors_vmo_ = descriptors_vmo.release();

  return ZX_OK;
}

void NetworkDeviceClient::AttachPort(port_id_t port_id, fbl::Vector<frame_type_t> rx_frame_types,
                                     ErrorCallback callback) {
  auto result = session_->Attach(port_id, rx_frame_types);
  callback(result);
}

void NetworkDeviceClient::DetachPort(port_id_t port_id, ErrorCallback callback) {
  auto result = session_->Detach(port_id);
  callback(result);
}

void NetworkDeviceClient::GetPortInfoWithMac(
    port_id_t port_id, NetworkDeviceClient::PortInfoWithMacCallback callback) {
  struct State {
    PortInfoAndMac result;
    network::internal::DevicePort* port_client;
    ddk::MacAddrProtocolClient mac_client;
  };

  fbl::AllocChecker ac;
  auto state = fbl::make_unique_checked<State>(&ac);
  if (!ac.check()) {
    callback(zx::error(ZX_ERR_NO_MEMORY));
    return;
  }

  // Connect to the requested port.
  device_->GetPort(port_id, &state->port_client);
  if (!state->port_client) {
    callback(zx::error(ZX_ERR_BAD_STATE));
    return;
  }

  // Connect to the port's MacAddressing interface.
  mac_addr_protocol_t* out_mac_ifc = nullptr;
  state->port_client->GetMac(&out_mac_ifc);

  if (!out_mac_ifc) {
    callback(zx::error(ZX_ERR_BAD_STATE));
    return;
  }
  state->mac_client = ddk::MacAddrProtocolClient(out_mac_ifc);

  port_base_info_t info;
  state->port_client->GetInfo(&info);

  port_info_t pinfo = {
      .id = port_id,
      .base_info = info,
  };

  zx::result<PortInfoAndMac> port_info =
      PortInfoAndMac::Create(pinfo, /*unicast_address=*/std::nullopt);

  if (port_info.is_error()) {
    callback(zx::error(port_info.error_value()));
    return;
  }

  // Get the Mac address of the interface.
  mac_address_t mac_address;
  state->mac_client.GetAddress(&mac_address);
  port_info.value().unicast_address = mac_address;

  callback(zx::success(std::move(port_info.value())));
}

void NetworkDeviceClient::GetPorts(PortsCallback callback) {
  struct PortWatcherHelper {
    static zx::result<fbl::Vector<port_id_t>> Watch(network::internal::PortWatcher* watcher,
                                                    fbl::Vector<port_id_t> found_ports) {
      zx::result<bool> complete;
      watcher->Watch([&complete, &ports = found_ports](
                         zx::result<std::pair<uint8_t, device_port_event_t>> result) mutable {
        if (result.is_error()) {
          return;
        }
        auto [type, event] = result.value();
        switch (type) {
          case EVENT_TYPE_IDLE:
            complete = zx::ok(true);
            break;
          case EVENT_TYPE_EXISTING: {
            fbl::AllocChecker ac;
            ports.push_back(event.existing, &ac);
            if (!ac.check()) {
              return;
            }
            complete = zx::ok(false);
            break;
          }
          case EVENT_TYPE_REMOVED:
          case EVENT_TYPE_ADDED:
            complete = zx::error(ZX_ERR_INTERNAL);
            break;
        }
      });

      if (complete.is_error()) {
        return zx::error(complete.error_value());
      }
      if (complete.value()) {
        return zx::ok(std::move(found_ports));
      }
      return Watch(watcher, std::move(found_ports));
    }
  };

  auto watcher = device_->GetPortWatcher();
  if (!watcher) {
    callback(zx::error(ZX_ERR_BAD_STATE));
    return;
  }

  auto ports = PortWatcherHelper::Watch(watcher, {});
  if (ports.is_error()) {
    callback(zx::error(ports.error_value()));
    return;
  }
  callback(zx::success(std::move(ports.value())));
}

zx_status_t NetworkDeviceClient::KillSession() {
  if (!session_) {
    return ZX_ERR_BAD_STATE;
  }
  // Cancel all the waits so we stop fetching frames.
  // rx_wait_.Cancel();
  // rx_writable_wait_.Cancel();
  // tx_wait_.Cancel();
  // tx_writable_wait_.Cancel();

  // const fidl::Status result = session_->Close();
  // if (result.is_peer_closed()) {
  //   return ZX_OK;
  // }
  // return result.status();
  return ZX_OK;
}

zx::result<std::unique_ptr<NetworkDeviceClient::StatusWatchHandle>>
NetworkDeviceClient::WatchStatus(port_id_t port_id, StatusCallback callback, uint32_t buffer) {
  network::internal::DevicePort* port_client = nullptr;
  device_->GetPort(port_id, &port_client);
  if (!port_client) {
    return zx::error(ZX_ERR_BAD_STATE);
  }

  auto status_watch = port_client->GetStatusWatcher(buffer);
  if (!status_watch) {
    return zx::error(ZX_ERR_BAD_STATE);
  }

  fbl::AllocChecker ac;
  auto handle = std::unique_ptr<NetworkDeviceClient::StatusWatchHandle>(
      new (&ac) NetworkDeviceClient::StatusWatchHandle(status_watch, std::move(callback)));
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  return zx::ok(std::move(handle));
}

zx::result<session_info> NetworkDeviceClient::MakeSessionInfo() {
  uint64_t descriptor_length_words = session_config_.descriptor_length / sizeof(uint64_t);
  ZX_DEBUG_ASSERT_MSG(descriptor_length_words <= std::numeric_limits<uint8_t>::max(),
                      "session descriptor length %ld (%ld words) overflows uint8_t",
                      session_config_.descriptor_length, descriptor_length_words);

  ZX_ASSERT(descriptors_vmo_);
  ZX_ASSERT(data_vmo_);

  session_info session_info = {
      .descriptors_list = (uint8_t*)fbl::ExportToRawPtr<VmObjectDispatcher>(&descriptors_vmo_),
      .descriptors_count = 1,
      .data_list = (uint8_t*)fbl::ExportToRawPtr<VmObjectDispatcher>(&data_vmo_),
      .data_count = 1,
      .descriptor_version = NETWORK_DEVICE_DESCRIPTOR_VERSION,
      .descriptor_length = static_cast<uint8_t>(descriptor_length_words),
      .descriptor_count = descriptor_count_,
      .options = session_config_.options,
  };
  return zx::ok(session_info);
}

buffer_descriptor_t* NetworkDeviceClient::descriptor(uint16_t idx) {
  ZX_ASSERT_MSG(idx < descriptor_count_, "invalid index %d, want < %d", idx, descriptor_count_);
  ZX_ASSERT_MSG(descriptors_.start() != nullptr, "descriptors not mapped");
  return reinterpret_cast<buffer_descriptor_t*>(static_cast<uint8_t*>(descriptors_.start()) +
                                                session_config_.descriptor_length * idx);
}

void* NetworkDeviceClient::data(uint64_t offset) {
  ZX_ASSERT(offset < data_.size());
  return static_cast<uint8_t*>(data_.start()) + offset;
}

void NetworkDeviceClient::ResetRxDescriptor(buffer_descriptor_t* descriptor) {
  *descriptor = {
      .nxt = 0xFFFF,
      .info_type = static_cast<uint32_t>(INFO_TYPE_NO_INFO),
      .offset = descriptor->offset,
      .data_length = session_config_.buffer_length,
  };
}

void NetworkDeviceClient::ResetTxDescriptor(buffer_descriptor_t* descriptor) {
  *descriptor = {
      .nxt = 0xFFFF,
      .info_type = static_cast<uint32_t>(INFO_TYPE_NO_INFO),
      .offset = descriptor->offset,
      .head_length = session_config_.tx_header_length,
      .tail_length = session_config_.tx_tail_length,
      .data_length = session_config_.buffer_length - session_config_.tx_header_length -
                     session_config_.tx_tail_length,
  };
}

zx_status_t NetworkDeviceClient::PrepareDescriptors() {
  uint16_t desc = 0;
  uint64_t buff_off = 0;
  auto* pDesc = static_cast<uint8_t*>(descriptors_.start());
  fbl::AllocChecker ac;
  rx_out_queue_.reserve(session_config_.rx_descriptor_count, &ac);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  tx_out_queue_.reserve(session_config_.tx_descriptor_count, &ac);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  for (; desc < session_config_.rx_descriptor_count; desc++) {
    auto* descriptor = reinterpret_cast<buffer_descriptor_t*>(pDesc);
    descriptor->offset = buff_off;
    ResetRxDescriptor(descriptor);

    buff_off += session_config_.buffer_stride;
    pDesc += session_config_.descriptor_length;
    rx_out_queue_.push_back(desc, &ac);
    if (!ac.check()) {
      return ZX_ERR_NO_MEMORY;
    }
  }
  for (; desc < descriptor_count_; desc++) {
    auto* descriptor = reinterpret_cast<buffer_descriptor_t*>(pDesc);
    ResetTxDescriptor(descriptor);
    descriptor->offset = buff_off;

    buff_off += session_config_.buffer_stride;
    pDesc += session_config_.descriptor_length;
    tx_avail_.push_back(desc, &ac);
    if (!ac.check()) {
      return ZX_ERR_NO_MEMORY;
    }
  }

  // rx_wait_.set_object(rx_fifo_.get());
  // rx_wait_.set_trigger(kFifoWaitReads);
  rx_wait_.handle = Handle::Make(rx_fifo_.dispatcher(), ZX_DEFAULT_FIFO_RIGHTS);
  // ZX_ASSERT(rx_wait_.Begin(dispatcher_) == ZX_OK);
  rx_wait_.pending = true;
  ZX_ASSERT(port_->MakeObserver(0, rx_wait_.handle.get(), kRxKey, kFifoWaitReads) == ZX_OK);

  // tx_wait_.set_object(tx_fifo_.get());
  // tx_wait_.set_trigger(kFifoWaitReads);
  tx_wait_.handle = Handle::Make(tx_fifo_.dispatcher(), ZX_DEFAULT_FIFO_RIGHTS);
  // ZX_ASSERT(tx_wait_.Begin(dispatcher_) == ZX_OK)
  tx_wait_.pending = true;
  ZX_ASSERT(port_->MakeObserver(0, tx_wait_.handle.get(), kTxKey, kFifoWaitReads) == ZX_OK);

  // rx_writable_wait_.set_object(rx_fifo_.get());
  // rx_writable_wait_.set_trigger(kFifoWaitWrites);
  rx_writable_wait_.handle = Handle::Make(rx_fifo_.dispatcher(), ZX_DEFAULT_FIFO_RIGHTS);

  // tx_writable_wait_.set_object(tx_fifo_.get());
  // tx_writable_wait_.set_trigger(kFifoWaitWrites);
  tx_writable_wait_.handle = Handle::Make(tx_fifo_.dispatcher(), ZX_DEFAULT_FIFO_RIGHTS);

  FlushRx();

  return ZX_OK;
}

void NetworkDeviceClient::FlushRx() {
  LTRACE_ENTRY;

  size_t flush = std::min(rx_out_queue_.size(), static_cast<size_t>(device_info_.rx_depth));
  ZX_ASSERT(flush != 0);

  // TODO(https://fxbug.dev/42107145): We're assuming that writing to the FIFO here
  // is a sufficient memory barrier for the other end to access the data. That
  // is currently true but not really guaranteed by the API.
  size_t actual;
  zx_status_t status = rx_fifo_.dispatcher()->Write(sizeof(uint16_t),
                                                    (uint8_t*)rx_out_queue_.data(), flush, &actual);
  bool sched_more;
  if (status == ZX_OK) {
    // rx_out_queue_.erase(rx_out_queue_.begin(), rx_out_queue_.begin() + flush);
    for (uint16_t i = 0; i < flush; i++) {
      rx_out_queue_.erase(0);
    }
    sched_more = !rx_out_queue_.is_empty();
  } else {
    sched_more = status == ZX_ERR_SHOULD_WAIT;
  }

  if (sched_more && !rx_writable_wait_.is_pending()) {
    rx_writable_wait_.pending = true;
    ZX_ASSERT(port_->MakeObserver(0, rx_writable_wait_.handle.get(), kRxWritableKey,
                                  kFifoWaitWrites) == ZX_OK);
  }
}

void NetworkDeviceClient::FlushTx() {
  LTRACE_ENTRY;

  size_t flush = std::min(tx_out_queue_.size(), static_cast<size_t>(device_info_.tx_depth));
  ZX_ASSERT(flush != 0);

  // TODO(https://fxbug.dev/42107145): We're assuming that writing to the FIFO here
  // is a sufficient memory barrier for the other end to access the data. That
  // is currently true but not really guaranteed by the API.
  size_t actual;
  zx_status_t status = tx_fifo_.dispatcher()->Write(sizeof(uint16_t),
                                                    (uint8_t*)tx_out_queue_.data(), flush, &actual);
  bool sched_more;
  if (status == ZX_OK) {
    // tx_out_queue_.erase(tx_out_queue_.begin(), tx_out_queue_.begin() + flush);
    for (uint16_t i = 0; i < flush; i++) {
      tx_out_queue_.erase(0);
    }
    sched_more = !tx_out_queue_.is_empty();
  } else {
    sched_more = status == ZX_ERR_SHOULD_WAIT;
  }

  if (sched_more && !tx_writable_wait_.is_pending()) {
    tx_writable_wait_.pending = true;
    ZX_ASSERT(port_->MakeObserver(0, tx_writable_wait_.handle.get(), kTxWritableKey,
                                  kFifoWaitWrites) == ZX_OK);
  }
}

void NetworkDeviceClient::ErrorTeardown(zx_status_t err) {
  LTRACEF("err: %s\n", zx_status_get_string(err));
  session_running_ = false;
  data_.Unmap();
  data_vmo_.reset();
  descriptors_.Unmap();
  descriptors_vmo_.reset();
  session_ = {};

  bool zero_handles = port_->decrement_handle_count();
  if (zero_handles) {
    port_->on_zero_handles();
  }
  port_.reset();

#if 0
  auto cancel_wait = [](async::WaitBase& wait, const char* name) {
    zx_status_t status = wait.Cancel();
    switch (status) {
      case ZX_OK:
      case ZX_ERR_NOT_FOUND:
        break;
      default:
        FX_PLOGS(ERROR, status) << "failed to cancel" << name;
    }
  };
  //cancel_wait(tx_wait_, "tx_wait");
  //cancel_wait(rx_wait_, "rx_wait");
  //cancel_wait(tx_writable_wait_, "tx_writable_wait");
  //cancel_wait(rx_writable_wait_, "rx_writable_wait");
#endif

  if (err_callback_) {
    err_callback_(err);
  }
}

void NetworkDeviceClient::TxSignal(zx_status_t status, Handle* wait,
                                   const zx_packet_signal_t* signal) {
  if (status != ZX_OK) {
    LTRACEF("tx wait failed: %s\n", zx_status_get_string(status));
    return;
  }
  if (signal->observed & signal->trigger & ZX_FIFO_PEER_CLOSED) {
    LTRACEF("tx fifo was closed\n");
    ErrorTeardown(ZX_ERR_PEER_CLOSED);
    return;
  }
  if (signal->observed & signal->trigger & ZX_FIFO_READABLE) {
    FetchTx();
  }
  if ((signal->observed & signal->trigger & ZX_FIFO_WRITABLE) && !tx_out_queue_.is_empty()) {
    FlushTx();
  }

  if (wait != tx_writable_wait_.handle.get() || !tx_out_queue_.is_empty()) {
    ZX_ASSERT(port_->MakeObserver(0, tx_wait_.handle.get(), kTxKey, kFifoWaitReads) == ZX_OK);
    tx_wait_.pending = true;
  }
}

void NetworkDeviceClient::RxSignal(zx_status_t status, Handle* wait,
                                   const zx_packet_signal_t* signal) {
  if (status != ZX_OK) {
    LTRACEF("rx wait failed: %s\n", zx_status_get_string(status));
    return;
  }

  if (signal->observed & signal->trigger & ZX_FIFO_PEER_CLOSED) {
    LTRACEF("rx fifo was closed\n");
    ErrorTeardown(ZX_ERR_PEER_CLOSED);
    return;
  }

  if (signal->observed & signal->trigger & ZX_FIFO_READABLE) {
    FetchRx();
  }

  if ((signal->observed & signal->trigger & ZX_FIFO_WRITABLE) && !rx_out_queue_.is_empty()) {
    FlushRx();
  }

  if (wait != rx_writable_wait_.handle.get() || !rx_out_queue_.is_empty()) {
    ZX_ASSERT(port_->MakeObserver(0, rx_wait_.handle.get(), kRxKey, kFifoWaitReads) == ZX_OK);
    rx_wait_.pending = true;
  }
}

void NetworkDeviceClient::FetchRx() {
  uint16_t buff[kMaxDepth];
  size_t read;
  zx_status_t status;
  if ((status = rx_fifo_.dispatcher()->Read(sizeof(uint16_t), (uint8_t*)buff, kMaxDepth, &read)) !=
      ZX_OK) {
    LTRACEF("Error reading from rx queue: %s\n", zx_status_get_string(status));
    return;
  }
  uint16_t* desc_idx = buff;
  while (read > 0) {
    if (rx_callback_) {
      rx_callback_(Buffer(this, *desc_idx, true));
    } else {
      ReturnRxDescriptor(*desc_idx);
    }

    read--;
    desc_idx++;
  }
}

zx_status_t NetworkDeviceClient::Send(NetworkDeviceClient::Buffer* buffer) {
  if (!buffer->is_valid()) {
    return ZX_ERR_UNAVAILABLE;
  }
  if (buffer->rx_) {
    // If this is an RX buffer, we need to get a TX buffer from the pool and return it as an RX
    // buffer in place of this.
    auto tx_buffer = AllocTx();
    if (!tx_buffer.is_valid()) {
      return ZX_ERR_NO_RESOURCES;
    }
    // Flip the buffer, it'll be returned to the rx queue on destruction.
    tx_buffer.rx_ = true;
    buffer->rx_ = false;
  }
  if (!tx_writable_wait_.is_pending()) {
    zx_status_t status =
        port_->MakeObserver(0, tx_writable_wait_.handle.get(), kTxWritableKey, kFifoWaitWrites);
    if (status != ZX_OK) {
      return status;
    }
  }

  fbl::AllocChecker ac;
  tx_out_queue_.push_back(buffer->descriptor_, &ac);
  ZX_ASSERT(ac.check());

  // Don't return this buffer on destruction.
  // Also invalidate it.
  buffer->parent_ = nullptr;
  return ZX_OK;
}

void NetworkDeviceClient::ReturnTxDescriptor(uint16_t idx) {
  auto* desc = descriptor(idx);
  if (desc->chain_length != 0) {
    ReturnTxDescriptor(desc->nxt);
  }
  ResetTxDescriptor(desc);
  fbl::AllocChecker ac;
  tx_avail_.push_back(idx, &ac);
  ZX_ASSERT(ac.check());
}

void NetworkDeviceClient::ReturnRxDescriptor(uint16_t idx) {
  auto* desc = descriptor(idx);
  if (desc->chain_length != 0) {
    ReturnRxDescriptor(desc->nxt);
  }
  ResetRxDescriptor(desc);
  fbl::AllocChecker ac;
  rx_out_queue_.push_back(idx, &ac);
  ZX_ASSERT(ac.check());

  if (!rx_writable_wait_.is_pending()) {
    ZX_ASSERT(port_->MakeObserver(0, rx_writable_wait_.handle.get(), kRxWritableKey,
                                  kFifoWaitWrites) == ZX_OK);
  }
}

void NetworkDeviceClient::FetchTx() {
  uint16_t buff[kMaxDepth];
  size_t read;
  zx_status_t status;
  if ((status = tx_fifo_.dispatcher()->Read(sizeof(uint16_t), (uint8_t*)buff, kMaxDepth, &read)) !=
      ZX_OK) {
    LTRACEF("Error reading from tx queue: %s\n", zx_status_get_string(status));
    return;
  }
  uint16_t* desc_idx = buff;
  while (read > 0) {
    // TODO count and log tx errors
    ReturnTxDescriptor(*desc_idx);
    read--;
    desc_idx++;
  }
}

int NetworkDeviceClient::Thread(void* arg) {
  NetworkDeviceClient* thiz = static_cast<NetworkDeviceClient*>(arg);
  for (;;) {
    zx_port_packet_t packet;
    zx_status_t status = thiz->port_->Dequeue(Deadline::infinite(), &packet);
    if (status != ZX_OK) {
      LTRACEF("wait failed: %s\n", zx_status_get_string(status));
      break;
    }

    if (packet.type == ZX_PKT_TYPE_SIGNAL_ONE) {
      switch (packet.key) {
        case kTxKey: {
          thiz->tx_wait_.pending = false;
          thiz->TxSignal(ZX_OK, thiz->tx_wait_.handle.get(), &packet.signal);
          break;
        }
        case kRxKey: {
          thiz->rx_wait_.pending = false;
          thiz->RxSignal(ZX_OK, thiz->rx_wait_.handle.get(), &packet.signal);
          break;
        }
        case kTxWritableKey: {
          thiz->tx_writable_wait_.pending = false;
          thiz->TxSignal(ZX_OK, thiz->tx_writable_wait_.handle.get(), &packet.signal);
          break;
        }
        case kRxWritableKey: {
          thiz->rx_writable_wait_.pending = false;
          thiz->RxSignal(ZX_OK, thiz->rx_writable_wait_.handle.get(), &packet.signal);
          break;
        }
      }
    }

    if (!thiz->session_running_) {
      break;
    }
  }
  return 0;
}

NetworkDeviceClient::Buffer NetworkDeviceClient::AllocTx() {
  if (tx_avail_.is_empty()) {
    return Buffer();
  } else {
    auto idx = tx_avail_.erase(0);
    return Buffer(this, idx, false);
  }
}

NetworkDeviceClient::Buffer::Buffer() : parent_(nullptr), descriptor_(0), rx_(false) {}

NetworkDeviceClient::Buffer::Buffer(NetworkDeviceClient* parent, uint16_t descriptor, bool rx)
    : parent_(parent), descriptor_(descriptor), rx_(rx) {}

NetworkDeviceClient::Buffer::Buffer(NetworkDeviceClient::Buffer&& other) noexcept
    : parent_(other.parent_),
      descriptor_(other.descriptor_),
      rx_(other.rx_),
      data_(std::move(other.data_)) {
  other.parent_ = nullptr;
}

NetworkDeviceClient::Buffer::~Buffer() {
  if (parent_) {
    if (rx_) {
      parent_->ReturnRxDescriptor(descriptor_);
    } else {
      parent_->ReturnTxDescriptor(descriptor_);
    }
  }
}

NetworkDeviceClient::BufferData& NetworkDeviceClient::Buffer::data() {
  ZX_ASSERT(is_valid());
  if (!data_.is_loaded()) {
    data_.Load(parent_, descriptor_);
  }
  return data_;
}

const NetworkDeviceClient::BufferData& NetworkDeviceClient::Buffer::data() const {
  ZX_ASSERT(is_valid());
  if (!data_.is_loaded()) {
    data_.Load(parent_, descriptor_);
  }
  return data_;
}

zx_status_t NetworkDeviceClient::Buffer::Send() {
  if (!is_valid()) {
    return ZX_ERR_UNAVAILABLE;
  }
  zx_status_t status = data_.PadTo(parent_->device_info_.min_tx_buffer_length);
  if (status != ZX_OK) {
    return status;
  }
  return parent_->Send(this);
}

void NetworkDeviceClient::BufferData::Load(NetworkDeviceClient* parent, uint16_t idx) {
  auto* desc = parent->descriptor(idx);
  while (desc) {
    auto& cur = parts_[parts_count_];
    cur.base_ = parent->data(desc->offset + desc->head_length);
    cur.desc_ = desc;
    parts_count_++;
    if (desc->chain_length != 0) {
      desc = parent->descriptor(desc->nxt);
    } else {
      desc = nullptr;
    }
  }
}

NetworkDeviceClient::BufferRegion& NetworkDeviceClient::BufferData::part(size_t idx) {
  ZX_ASSERT(idx < parts_count_);
  return parts_[idx];
}

const NetworkDeviceClient::BufferRegion& NetworkDeviceClient::BufferData::part(size_t idx) const {
  ZX_ASSERT(idx < parts_count_);
  return parts_[idx];
}

uint32_t NetworkDeviceClient::BufferData::len() const {
  uint32_t c = 0;
  for (uint32_t i = 0; i < parts_count_; i++) {
    c += parts_[i].len();
  }
  return c;
}

frame_type_t NetworkDeviceClient::BufferData::frame_type() const {
  return static_cast<frame_type_t>(part(0).desc_->frame_type);
}

void NetworkDeviceClient::BufferData::SetFrameType(frame_type_t type) {
  part(0).desc_->frame_type = static_cast<uint8_t>(type);
}

port_id_t NetworkDeviceClient::BufferData::port_id() const {
  const buffer_descriptor_t& desc = *part(0).desc_;
  return {
      .base = desc.port_id.base,
      .salt = desc.port_id.salt,
  };
}

void NetworkDeviceClient::BufferData::SetPortId(port_id_t port_id) {
  buffer_descriptor_t& desc = *part(0).desc_;
  desc.port_id = {
      .base = port_id.base,
      .salt = port_id.salt,
  };
}

info_type_t NetworkDeviceClient::BufferData::info_type() const {
  return static_cast<info_type_t>(part(0).desc_->frame_type);
}

uint32_t NetworkDeviceClient::BufferData::inbound_flags() const {
  return part(0).desc_->inbound_flags;
}

uint32_t NetworkDeviceClient::BufferData::return_flags() const {
  return part(0).desc_->return_flags;
}

void NetworkDeviceClient::BufferData::SetTxRequest(tx_flags_t tx_flags) {
  part(0).desc_->inbound_flags = static_cast<uint32_t>(tx_flags);
}

size_t NetworkDeviceClient::BufferData::Write(const void* src, size_t len) {
  const auto* ptr = static_cast<const uint8_t*>(src);
  size_t written = 0;
  for (uint32_t i = 0; i < parts_count_; i++) {
    auto& part = parts_[i];
    uint32_t wr = std::min(static_cast<uint32_t>(len - written), part.len());
    part.Write(ptr, wr);
    ptr += wr;
    written += wr;
  }
  return written;
}

size_t NetworkDeviceClient::BufferData::Write(const BufferData& data) {
  size_t count = 0;

  size_t idx_me = 0;
  size_t offset_me = 0;
  size_t offset_other = 0;
  for (size_t idx_o = 0; idx_o < data.parts_count_ && idx_me < parts_count_;) {
    size_t wr = parts_[idx_me].Write(offset_me, data.parts_[idx_o], offset_other);
    offset_me += wr;
    offset_other += wr;
    count += wr;
    if (offset_me >= parts_[idx_me].len()) {
      idx_me++;
      offset_me = 0;
    }
    if (offset_other >= data.parts_[idx_o].len()) {
      idx_o++;
      offset_other = 0;
    }
  }
  // Update the length on the last descriptor.
  if (idx_me < parts_count_) {
    ZX_DEBUG_ASSERT(offset_me <= std::numeric_limits<uint32_t>::max());
    parts_[idx_me].CapLength(static_cast<uint32_t>(offset_me));
  }

  return count;
}

zx_status_t NetworkDeviceClient::BufferData::PadTo(size_t size) {
  size_t total_size = 0;
  for (uint32_t i = 0; i < parts_count_ && total_size < size; i++) {
    total_size += parts_[i].PadTo(size - total_size);
  }
  if (total_size < size) {
    return ZX_ERR_BUFFER_TOO_SMALL;
  }
  return ZX_OK;
}

size_t NetworkDeviceClient::BufferData::Read(void* dst, size_t len) const {
  auto* ptr = static_cast<uint8_t*>(dst);
  size_t actual = 0;
  for (uint32_t i = 0; i < parts_count_ && len > 0; i++) {
    auto& part = parts_[i];
    size_t rd = part.Read(ptr, len);
    len -= rd;
    ptr += rd;
    actual += rd;
  }
  return actual;
}

void NetworkDeviceClient::BufferRegion::CapLength(uint32_t len) {
  if (len <= desc_->data_length) {
    desc_->tail_length += desc_->data_length - len;
    desc_->data_length = len;
  }
}

uint32_t NetworkDeviceClient::BufferRegion::len() const { return desc_->data_length; }

cpp20::span<uint8_t> NetworkDeviceClient::BufferRegion::data() {
  return cpp20::span(static_cast<uint8_t*>(base_), len());
}

cpp20::span<const uint8_t> NetworkDeviceClient::BufferRegion::data() const {
  return cpp20::span(static_cast<const uint8_t*>(base_), len());
}

size_t NetworkDeviceClient::BufferRegion::Write(const void* src, size_t len, size_t offset) {
  uint32_t nlen = std::min(desc_->data_length, static_cast<uint32_t>(len + offset));
  CapLength(nlen);
  std::copy_n(static_cast<const uint8_t*>(src), this->len() - offset, data().begin() + offset);
  return this->len();
}

size_t NetworkDeviceClient::BufferRegion::Read(void* dst, size_t len, size_t offset) const {
  if (offset >= desc_->data_length) {
    return 0;
  }
  len = std::min(len, desc_->data_length - offset);
  std::copy_n(data().begin() + offset, len, static_cast<uint8_t*>(dst));
  return len;
}

size_t NetworkDeviceClient::BufferRegion::Write(size_t offset, const BufferRegion& src,
                                                size_t src_offset) {
  if (offset >= desc_->data_length || src_offset >= src.desc_->data_length) {
    return 0;
  }
  size_t wr = std::min(desc_->data_length - offset, src.desc_->data_length - src_offset);
  std::copy_n(src.data().begin() + src_offset, wr, data().begin() + offset);
  return wr;
}

size_t NetworkDeviceClient::BufferRegion::PadTo(size_t size) {
  if (size > desc_->data_length) {
    size -= desc_->data_length;
    cpp20::span<uint8_t> pad(static_cast<uint8_t*>(base_) + desc_->head_length + desc_->data_length,
                             std::min(size, static_cast<size_t>(desc_->tail_length)));
    memset(pad.data(), 0x00, pad.size());
    desc_->data_length += pad.size();
    desc_->tail_length -= pad.size();
  }
  return desc_->data_length;
}

void NetworkDeviceClient::StatusWatchHandle::Watch() {
  watcher_->WatchStatus([this](port_status_t status) {
    callback_(zx::ok(status));
    // Watch again, we only stop watching when StatusWatchHandle is destroyed.
    // Watch();
  });
}

}  // namespace client
}  // namespace network
