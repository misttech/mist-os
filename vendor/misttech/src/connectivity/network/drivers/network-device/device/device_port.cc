// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "device_port.h"

#include <algorithm>

// #include <fidl/fuchsia.hardware.network.driver/cpp/fidl.h>
// #include <lib/async/cpp/task.h>

#include <lib/mistos/util/back_insert_iterator.h>
#include <trace.h>

#include "device_interface.h"

#define LOCAL_TRACE 2

namespace network::internal {

void DevicePort::Create(DeviceInterface* parent, port_id_t id, ddk::NetworkPortProtocolClient port,
                        TeardownCallback&& on_teardown, OnCreated&& on_created) {
  if (parent == nullptr) {
    TRACEF("null parent provided\n");
    on_created(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }

  fbl::AllocChecker ac;
  std::unique_ptr<DevicePort> device_port(new (&ac)
                                              DevicePort(parent, id, port, std::move(on_teardown)));
  if (!ac.check()) {
    TRACEF("Failed to allocate memory for port\n");
    on_created(zx::error(ZX_ERR_NO_MEMORY));
    return;
  }

  // Keep a raw pointer for making the call below, the unique ptr will have been moved.
  DevicePort* port_ptr = device_port.get();
  port_ptr->Init([&on_created, port = std::move(device_port)](zx_status_t status) mutable {
    if (status != ZX_OK) {
      // Reset the port client to ensure that the DevicePort object doesn't try to do anything
      // with it on destruction.
      port->port_ = ddk::NetworkPortProtocolClient();
      on_created(zx::error(status));
      return;
    }
    on_created(zx::ok(std::move(port)));
  });
}

DevicePort::DevicePort(DeviceInterface* parent, port_id_t id, ddk::NetworkPortProtocolClient port,
                       TeardownCallback&& on_teardown)
    : parent_(parent), id_(id), port_(std::move(port)), on_teardown_(std::move(on_teardown)) {
  if (parent_) {
  }
}

DevicePort::~DevicePort() {
#if 0
  fdf::Arena arena('NETD');
  if (port_.is_valid()) {
    fidl::OneWayStatus status = port_.buffer(arena)->Removed();
    if (!status.ok()) {
      TRACEF("Failed to remove port: %s\n", status.FormatDescription().c_str());
    }
  }
#endif
}

void DevicePort::Init(fit::callback<void(zx_status_t)>&& on_complete) {
  GetMac([this, &on_complete](zx::result<ddk::MacAddrProtocolClient> result) mutable {
    if (result.is_error()) {
      on_complete(result.status_value());
      return;
    }

    // Pre-create the callback here because it needs to be shared between two separate paths below
    // and the on_complete callback can only be captured in one place.
    fit::callback<void(zx_status_t)> get_port_info = [this,
                                                      &on_complete](zx_status_t status) mutable {
      if (status != ZX_OK) {
        on_complete(status);
        return;
      }
      GetInitialPortInfo([this, &on_complete](zx_status_t status) mutable {
        if (status != ZX_OK) {
          on_complete(status);
          return;
        }
        GetInitialStatus([&on_complete](zx_status_t status) mutable { on_complete(status); });
      });
    };

    if (result.value().is_valid()) {
      CreateMacInterface(std::move(result.value()), std::move(get_port_info));
      return;
    }
    get_port_info(ZX_OK);
  });
}

void DevicePort::GetMac(fit::callback<void(zx::result<ddk::MacAddrProtocolClient>)>&& on_complete) {
  mac_addr_protocol_t* proto = nullptr;
  port_.GetMac(&proto);
  if (proto == nullptr || proto->ctx == nullptr || proto->ops == nullptr) {
    // Mac protocol not implemented, return empty client end.
    LTRACEF("mac protocol not implemented\n");
    on_complete(zx::error(ZX_ERR_NOT_SUPPORTED));
    return;
  }
  ddk::MacAddrProtocolClient mac_addr(proto);
  on_complete(zx::ok(std::move(mac_addr)));
}

void DevicePort::CreateMacInterface(ddk::MacAddrProtocolClient&& mac_client,
                                    fit::callback<void(zx_status_t)>&& on_complete) {
  MacAddrDeviceInterface::Create(
      std::move(mac_client),
      [this, &on_complete](zx::result<std::unique_ptr<MacAddrDeviceInterface>> result) mutable {
        if (result.is_error()) {
          on_complete(result.status_value());
          return;
        }
        fbl::AutoLock lock(&lock_);
        mac_ = std::move(result.value());
        on_complete(ZX_OK);
      });
}

void DevicePort::GetInitialPortInfo(fit::callback<void(zx_status_t)>&& on_complete) {
  port_base_info_t info;
  port_.GetInfo(&info);

  if (info.port_class == 0) {
    TRACEF("missing port class\n");
    on_complete(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (info.rx_types_count > MAX_FRAME_TYPES) {
    TRACEF("too many port rx types: %ld > %d", info.rx_types_count, MAX_FRAME_TYPES);
    on_complete(ZX_ERR_INVALID_ARGS);
    return;
  }

  if (info.tx_types_count > MAX_FRAME_TYPES) {
    TRACEF("too many port tx types: %ld > %d", info.tx_types_count, MAX_FRAME_TYPES);
    on_complete(ZX_ERR_INVALID_ARGS);
    return;
  }

  port_class_ = info.port_class;

  ZX_ASSERT(supported_rx_.is_empty());
  if (info.rx_types_count > 0) {
    std::copy(info.rx_types_list, info.rx_types_list + info.rx_types_count,
              util::back_inserter(supported_rx_));
  }
  ZX_ASSERT(supported_tx_.is_empty());
  if (info.tx_types_count > 0) {
    std::copy(info.tx_types_list, info.tx_types_list + info.tx_types_count,
              util::back_inserter(supported_tx_));
  }
  on_complete(ZX_OK);
}

void DevicePort::GetInitialStatus(fit::callback<void(zx_status_t)>&& on_complete) {
  port_status_t status;
  port_.GetStatus(&status);
  status_ = status;
  on_complete(ZX_OK);
}

void DevicePort::StatusChanged(const port_status_t& new_status) {
  fbl::AutoLock lock(&lock_);
  for (auto& w : watchers_) {
    w.PushStatus(new_status);
  }
}

StatusWatcher* DevicePort::GetStatusWatcher(uint32_t buffer) {
  fbl::AutoLock lock(&lock_);
  if (teardown_started_) {
    // Don't install new watchers after teardown has started.
    return nullptr;
  }
  fbl::AllocChecker ac;
  auto n_watcher = fbl::make_unique_checked<StatusWatcher>(&ac, buffer);
  if (!ac.check()) {
    return nullptr;
  }

  port_status_t status;
  port_.GetStatus(&status);
  n_watcher->PushStatus(status);

  auto watcher_ptr = n_watcher.get();
  watchers_.push_back(std::move(n_watcher));
  return watcher_ptr;
}

bool DevicePort::MaybeFinishTeardown() {
  if (teardown_started_ && on_teardown_ && watchers_.is_empty() && !mac_) {
    //  Always finish teardown on dispatcher to evade deadlock opportunity on DeviceInterface ports
    //  lock.
    //  async::PostTask(dispatcher_, [this, call = std::move(on_teardown_)]() mutable { call(*this);
    //  });
    return true;
  }
  return false;
}

void DevicePort::Teardown() {
  fbl::AutoLock lock(&lock_);
  if (teardown_started_) {
    return;
  }
  teardown_started_ = true;
  // Attempt to conclude the teardown immediately if we have no live resources.
  if (MaybeFinishTeardown()) {
    return;
  }
#if 0
  for (auto& watcher : watchers_) {
    watcher.Unbind();
  }
  for (auto& binding : bindings_) {
    binding.Unbind();
  }
  if (mac_) {
    mac_->Teardown([this]() {
      // Always dispatch mac teardown callback to our dispatcher.
      async::PostTask(dispatcher_, [this]() {
        fbl::AutoLock lock(&lock_);
        // Dispose of mac entirely on teardown complete.
        mac_ = nullptr;
        MaybeFinishTeardown();
      });
    });
  }
#endif
}

#if 0
void DevicePort::GetMac(GetMacRequestView request, GetMacCompleter::Sync& _completer) {
  fidl::ServerEnd req = std::move(request->mac);

  fbl::AutoLock lock(&lock_);
  if (teardown_started_) {
    return;
  }
  if (!mac_) {
    req.Close(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  zx_status_t status = mac_->Bind(dispatcher_, std::move(req));
  if (status != ZX_OK) {
    LOGF_ERROR("failed to bind to MacAddr on port %d: %s", id_.base, zx_status_get_string(status));
  }
}
#endif

void DevicePort::SessionAttached() {
  fbl::AutoLock lock(&lock_);
  NotifySessionCount(++attached_sessions_count_);
}

void DevicePort::SessionDetached() {
  fbl::AutoLock lock(&lock_);
  ZX_ASSERT_MSG(attached_sessions_count_ > 0, "detached the same port twice");
  NotifySessionCount(--attached_sessions_count_);
}

void DevicePort::NotifySessionCount(size_t new_count) {
  if (teardown_started_) {
    // Skip all notifications if tearing down.
    return;
  }
  // Port active changes whenever the new count on session attaching or detaching edges away from
  // zero.
  if (new_count <= 1) {
    // Always post notifications for later on dispatcher so the port implementation can safely call
    // back into the core device with no risk of deadlocks.
    /*async::PostTask(dispatcher_, [this, active = new_count != 0]() {
      fdf::Arena arena('NETD');
      fidl::OneWayStatus result = port_.buffer(arena)->SetActive(active);
      if (!result.ok()) {
        LOGF_ERROR("SetActive failed with error: %s", result.FormatDescription().c_str());
      }
    });
    */
  }
}

bool DevicePort::IsValidRxFrameType(frame_type_t frame_type) const {
  cpp20::span rx_types(supported_rx_.begin(), supported_rx_.end());
  return std::ranges::any_of(rx_types,
                             [frame_type](const frame_type_t& t) { return t == frame_type; });
}

bool DevicePort::IsValidTxFrameType(frame_type_t frame_type) const {
  cpp20::span tx_types(supported_tx_.begin(), supported_tx_.end());
  return std::ranges::any_of(
      tx_types, [frame_type](const frame_type_support_t& t) { return t.type == frame_type; });
}

#if 0
void DevicePort::Bind(fidl::ServerEnd<netdev::Port> req) {
  fbl::AllocChecker ac;
  std::unique_ptr<Binding> binding(new (&ac) Binding);
  if (!ac.check()) {
    req.Close(ZX_ERR_NO_MEMORY);
    return;
  }

  fbl::AutoLock lock(&lock_);
  // Disallow binding a new request if teardown already started to prevent races
  // with the dispatched unbind below.
  if (teardown_started_) {
    return;
  }
  // Capture a pointer to the binding so we can erase it in the unbound function.
  Binding* binding_ptr = binding.get();
  binding->Bind(fidl::BindServer(dispatcher_, std::move(req), this,
                                 [binding_ptr](DevicePort* port, fidl::UnbindInfo /*unused*/,
                                               fidl::ServerEnd<netdev::Port> /*unused*/) {
      // Always complete unbind later to avoid deadlock in case bind
      // fails synchronously.
      async::PostTask(port->dispatcher_, [port, binding_ptr]() {
        fbl::AutoLock lock(&port->lock_);
        port->bindings_.erase(*binding_ptr);
        port->MaybeFinishTeardown();
      });
    }));

    bindings_.push_front(std::move(binding));
  }
#endif

void DevicePort::GetInfo(port_base_info_t* info) { port_.GetInfo(info); }
void DevicePort::GetStatus(port_status_t* status) { port_.GetStatus(status); }
void DevicePort::GetMac(mac_addr_protocol_t** out_mac_ifc) { port_.GetMac(out_mac_ifc); }

#if 0
void DevicePort::GetDevice(GetDeviceRequestView request, GetDeviceCompleter::Sync& _completer) {
  if (zx_status_t status = parent_->Bind(std::move(request->device)); status != ZX_OK) {
    LOGF_ERROR("bind failed %s", zx_status_get_string(status));
  }
}

  void DevicePort::Clone(CloneRequestView request, CloneCompleter::Sync & _completer) {
    Bind(std::move(request->port));
  }

  void DevicePort::GetCounters(GetCountersCompleter::Sync & completer) {
    fidl::Arena arena;

    netdev::wire::PortGetCountersResponse rsp =
        netdev::wire::PortGetCountersResponse::Builder(arena)
            .tx_frames(counters_.tx_frames)
            .tx_bytes(counters_.tx_bytes)
            .rx_frames(counters_.rx_frames)
            .rx_bytes(counters_.rx_bytes)
            .Build();
    completer.Reply(rsp);
  }

  void DevicePort::GetDiagnostics(GetDiagnosticsRequestView request,
                                  GetDiagnosticsCompleter::Sync & _completer) {
    parent_->diagnostics().Bind(std::move(request->diagnostics));
  }
#endif
}  // namespace network::internal
