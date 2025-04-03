// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "port_watcher.h"

#include <fbl/auto_lock.h>

#include <trace.h>

#define LOCAL_TRACE 0

namespace network::internal {

zx_status_t PortWatcher::Bind(cpp20::span<const port_id_t> existing_ports,
                              ClosedCallback closed_callback) {
  fbl::AutoLock lock(&lock_);
  // ZX_DEBUG_ASSERT(!binding_.has_value());

  // Gather all existing ports.
  for (const port_id_t& port_id : existing_ports) {
    Event event;
    event.SetExisting(port_id);
    if (zx_status_t status = QueueEvent(event); status != ZX_OK) {
      return status;
    }
  }
  Event idle;
  idle.SetIdle();
  if (zx_status_t status = QueueEvent(idle); status != ZX_OK) {
    return status;
  }

#if 0
  binding_ = fidl::BindServer(
      dispatcher, std::move(channel), this,
      [](PortWatcher* closed_ptr, fidl::UnbindInfo info,
         fidl::ServerEnd<netdev::PortWatcher> /*unused*/) {
        LOGF_TRACE("port watcher closed: %s", info.FormatDescription().c_str());
        PortWatcher& closed = *closed_ptr;
        fbl::AutoLock lock(&closed.lock_);
        closed.binding_.reset();
        std::optional pending_txn = std::exchange(closed.pending_txn_, std::nullopt);
        if (pending_txn.has_value()) {
          pending_txn.value().Close(ZX_ERR_CANCELED);
        }
        auto callback = std::exchange(closed.closed_cb_, nullptr);
        lock.release();
        if (callback) {
          callback(closed);
        }
      });
#endif
  closed_cb_ = std::move(closed_callback);
  return ZX_OK;
}

void PortWatcher::Unbind() {
  fbl::AutoLock lock(&lock_);
  // if (binding_.has_value()) {
  //   binding_->Unbind();
  // }
}

void PortWatcher::Watch(PortEventCallback&& callback) {
  LTRACE_ENTRY_OBJ;
  fbl::AutoLock lock(&lock_);
  if (event_queue_.is_empty()) {
    // if (pending_txn_.has_value()) {
    //  Can't enqueue more than one watch call.
    // completer.Close(ZX_ERR_BAD_STATE);
    // return;
    //}
    // pending_txn_ = completer.ToAsync();
    return;
  }

  std::unique_ptr event = event_queue_.pop_front();
  callback(zx::ok(
      std::pair<uint8_t, device_port_event_t>(event->event().event_type, event->event().event)));
}

zx_status_t PortWatcher::QueueEvent(const PortWatcher::Event& event) {
  LTRACEF("(%p)(%ld); queue = %ld\n", this, static_cast<long>(event.event().event_type),
          event_queue_.size());

  if (event_queue_.size() == kMaximumQueuedEvents) {
    return ZX_ERR_CANCELED;
  }
  ZX_ASSERT_MSG(event_queue_.size() < kMaximumQueuedEvents, "too many events in queue: %ld > %ld",
                event_queue_.size(), kMaximumQueuedEvents);
  fbl::AllocChecker ac;
  std::unique_ptr queueable = fbl::make_unique_checked<Event>(&ac, event);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  event_queue_.push_back(std::move(queueable));
  return ZX_OK;
}

void PortWatcher::PortAdded(port_id_t port_id) {
  fbl::AutoLock lock(&lock_);
  Event event;
  event.SetAdded(port_id);
  ProcessEvent(event);
}

void PortWatcher::PortRemoved(port_id_t port_id) {
  fbl::AutoLock lock(&lock_);
  Event event;
  event.SetRemoved(port_id);
  ProcessEvent(event);
}

void PortWatcher::ProcessEvent(const Event& event) {
  // std::optional txn = std::exchange(pending_txn_, std::nullopt);
  // if (txn.has_value()) {
  //   txn.value().Reply(event.event());
  //   return;
  //}
  zx_status_t status = QueueEvent(event);
  if (status != ZX_OK) {
  }
}

PortWatcher::Event::Event(const PortWatcher::Event& other) {
  switch (other.event_.event_type) {
    case EVENT_TYPE_EXISTING:
      SetExisting(other.port_id_);
      break;
    case EVENT_TYPE_ADDED:
      SetAdded(other.port_id_);
      break;
    case EVENT_TYPE_REMOVED:
      SetRemoved(other.port_id_);
      break;
    case EVENT_TYPE_IDLE:
      SetIdle();
      break;
  }
}

void PortWatcher::Event::SetExisting(port_id_t port_id) {
  port_id_ = port_id;
  event_.event_type = EVENT_TYPE_EXISTING;
  event_.event.existing = port_id_;
}

void PortWatcher::Event::SetAdded(port_id_t port_id) {
  port_id_ = port_id;
  event_.event_type = EVENT_TYPE_ADDED;
  event_.event.added = port_id_;
}

void PortWatcher::Event::SetRemoved(port_id_t port_id) {
  port_id_ = port_id;
  event_.event_type = EVENT_TYPE_REMOVED;
  event_.event.removed = port_id_;
}

void PortWatcher::Event::SetIdle() { event_.event_type = EVENT_TYPE_IDLE; }

}  // namespace network::internal
