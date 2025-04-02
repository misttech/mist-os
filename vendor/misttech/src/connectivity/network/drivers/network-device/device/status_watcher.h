// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_STATUS_WATCHER_H_
#define VENDOR_MISTTECH_SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_STATUS_WATCHER_H_

// #include <lib/async/dispatcher.h>
// #include <lib/fidl/cpp/wire/server.h>
// #include <lib/sync/completion.h>
// #include <threads.h>

// #include <queue>

#include <fbl/auto_lock.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/mutex.h>

#include "definitions.h"

namespace network::internal {

template <typename F>
void WithWireStatus(F fn, status_flags_t flags, uint32_t mtu) {
  port_status_t status = {.flags = flags, .mtu = mtu};
  fn(status);
}

class StatusWatcher : public fbl::DoublyLinkedListable<std::unique_ptr<StatusWatcher>> /*,
                       public fidl::WireServer<netdev::StatusWatcher>*/
{
 public:
  using StatusCallback = fit::callback<void(port_status_t)>;

  explicit StatusWatcher(uint32_t max_queue);
  ~StatusWatcher();

  void PushStatus(const port_status_t& status);

  void WatchStatus(StatusCallback callback);

 private:
  StatusWatcher(const StatusWatcher&) = delete;
  StatusWatcher& operator=(const StatusWatcher&) = delete;

  fbl::Mutex lock_;
  uint32_t max_queue_;

  // We need a value type to store the port status. It's possible to use FIDL types such as
  // WireTableFrames to do this but they're cumbersome to work with.
  struct PortStatus {
    explicit PortStatus(const port_status_t& ps) : flags(ps.flags), mtu(ps.mtu) {}
    bool operator==(const port_status_t& ps) const { return ps.flags == flags && ps.mtu == mtu; }
    status_flags_t flags;
    uint32_t mtu;
  };

  std::optional<PortStatus> last_observed_ __TA_GUARDED(lock_);
  fbl::Vector<PortStatus> queue_ __TA_GUARDED(lock_);
  // std::optional<WatchStatusCompleter::Async> pending_txn_ __TA_GUARDED(lock_);
  // std::optional<fidl::ServerBindingRef<netdev::StatusWatcher>> binding_ __TA_GUARDED(lock_);
  fit::callback<void(StatusWatcher*)> closed_cb_;
};

using StatusWatcherList = fbl::DoublyLinkedList<std::unique_ptr<StatusWatcher>>;

}  // namespace network::internal

#endif  // VENDOR_MISTTECH_SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_STATUS_WATCHER_H_
