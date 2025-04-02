// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_PORT_WATCHER_H_
#define VENDOR_MISTTECH_SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_PORT_WATCHER_H_

#include <fbl/intrusive_double_list.h>
#include <fbl/mutex.h>

#include "definitions.h"

namespace network::internal {

class PortWatcher : public fbl::DoublyLinkedListable<std::unique_ptr<PortWatcher>> /*,
 public fidl::WireServer<netdev::PortWatcher>*/
{
 public:
  using PortEventCallback =
      fit::callback<void(zx::result<std::pair<uint8_t, device_port_event_t>>)>;

  // Maximum number of port events that can be queued before the channel is closed.
  static constexpr size_t kMaximumQueuedEvents = MAX_PORTS * 2ul;
  using List = fbl::SizedDoublyLinkedList<std::unique_ptr<PortWatcher>>;
  using ClosedCallback = fit::callback<void(PortWatcher&)>;

  // Binds the watcher to |dispatcher| serving on |channel|.
  // |existing_ports| contains the port identifiers to be included in the watcher's existing ports
  // list.
  // |closed_callback| is called when the watcher is closed by the peer or by a call to |Unbind|.
  zx_status_t Bind(cpp20::span<const port_id_t> existing_portsl, ClosedCallback closed_callback);
  // Unbinds the port watcher if currently bound.
  void Unbind();

  // Notifies peer of port addition.
  void PortAdded(port_id_t port_id);
  // Notifies peer of port removal.
  void PortRemoved(port_id_t port_id);

  // FIDL protocol implementation.
  void Watch(PortEventCallback&& callback);

 private:
  // Helper class to provide owned FIDL port event union and intrusive linked list.
  class Event : public fbl::DoublyLinkedListable<std::unique_ptr<Event>> {
   public:
    Event() = default;
    Event(Event&& other) = delete;
    Event(const Event& other);

    void SetExisting(port_id_t port_id);
    void SetAdded(port_id_t port_id);
    void SetRemoved(port_id_t port_id);
    void SetIdle();

#define EVENT_TYPE_EXISTING UINT8_C(0x0)
#define EVENT_TYPE_ADDED UINT8_C(0x1)
#define EVENT_TYPE_REMOVED UINT8_C(0x2)
#define EVENT_TYPE_IDLE UINT8_C(0x3)
    struct DevicePortEvent {
      uint8_t event_type;
      device_port_event_t event;
    };

    DevicePortEvent event() const { return event_; }

   private:
    DevicePortEvent event_;
    // empty_t empty_;
    port_id_t port_id_;
  };

  // Queues an event in the internal queue.
  // Returns |ZX_ERR_NO_MEMORY| if it can't allocate queue space.
  // Returns |ZX_ERR_CANCELED| if too many events are already enqueued.
  [[nodiscard]] zx_status_t QueueEvent(const Event& event) __TA_REQUIRES(lock_);
  // Processes a single event, firing a pending FIDL response if one exists or enqueuing it for
  // later consumption.
  //
  // Closes the channel with an epitaph and unbinds on queueing errors.
  void ProcessEvent(const Event& event) __TA_REQUIRES(lock_);

  fbl::Mutex lock_;
  ClosedCallback closed_cb_;
  // std::optional<WatchCompleter::Async> pending_txn_ __TA_GUARDED(lock_);
  fbl::DoublyLinkedList<std::unique_ptr<Event>, fbl::DefaultObjectTag, fbl::SizeOrder::Constant>
      event_queue_ __TA_GUARDED(lock_);
  // std::optional<fidl::ServerBindingRef<netdev::PortWatcher>> binding_ __TA_GUARDED(lock_);
};

}  // namespace network::internal

#endif  // VENDOR_MISTTECH_SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_PORT_WATCHER_H_
