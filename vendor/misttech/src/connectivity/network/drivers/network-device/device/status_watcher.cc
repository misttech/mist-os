// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "status_watcher.h"

#include <zircon/status.h>

// #include "log.h"

namespace network::internal {

StatusWatcher::StatusWatcher(uint32_t max_queue) : max_queue_(max_queue) {
  if (max_queue_ == 0) {
    max_queue_ = 1;
  } else if (max_queue_ > MAX_STATUS_BUFFER) {
    max_queue_ = MAX_STATUS_BUFFER;
  }
}

#if 0
zx_status_t StatusWatcher::Bind(/*async_dispatcher_t* dispatcher,
                                fidl::ServerEnd<netdev::StatusWatcher> channel,*/
                                fit::callback<void(StatusWatcher*)> closed_callback) {
#if 0
  fbl::AutoLock lock(&lock_);
  ZX_DEBUG_ASSERT(!binding_.has_value());
  binding_ =
      fidl::BindServer(dispatcher, std::move(channel), this,
                       [](StatusWatcher* closed, fidl::UnbindInfo info,
                          fidl::ServerEnd<fuchsia_hardware_network::StatusWatcher> /*unused*/) {
                         LOGF_TRACE("watcher closed: %s", info.FormatDescription().c_str());
                         fbl::AutoLock lock(&closed->lock_);
                         closed->binding_.reset();
                         if (closed->pending_txn_.has_value()) {
                           closed->pending_txn_->Close(ZX_ERR_CANCELED);
                           closed->pending_txn_.reset();
                         }
                         if (closed->closed_cb_) {
                           lock.release();
                           closed->closed_cb_(closed);
                         }
                       });
#endif
  closed_cb_ = std::move(closed_callback);
  return ZX_OK;
}

void StatusWatcher::Unbind() {
#if 0
  fbl::AutoLock lock(&lock_);
  if (pending_txn_.has_value()) {
    pending_txn_->Close(ZX_ERR_CANCELED);
    pending_txn_.reset();
  }

  if (binding_.has_value()) {
    binding_->Unbind();
    binding_.reset();
  }
#endif
}
#endif
StatusWatcher::~StatusWatcher() {
  // ZX_ASSERT_MSG(!pending_txn_.has_value(),
  //              "tried to destroy StatusWatcher with a pending transaction");
  // ZX_ASSERT_MSG(!binding_.has_value(), "tried to destroy StatusWatcher without unbinding");
}

void StatusWatcher::WatchStatus(StatusCallback callback) {
  fbl::AutoLock lock(&lock_);
  if (queue_.is_empty()) {
    /*if (pending_txn_.has_value()) {
      if (last_observed_.has_value()) {
        // Complete the last pending transaction with the old value and retain the new completer as
        // an async transaction.
        WithWireStatus(
            [completer = std::move(std::exchange(pending_txn_, completer.ToAsync()).value())](
                netdev::wire::PortStatus wire_status) mutable { completer.Reply(wire_status); },
            last_observed_->flags, last_observed_->mtu);
      } else {
        // If we already have a pending transaction that hasn't been resolved and we don't have a
        // last observed value to give to it (meaning whoever created `StatusWatcher` scheduled it
        // without ever pushing any status information), we have no choice but to close the newer
        // completer.
        completer.Close(ZX_ERR_BAD_STATE);
      }
    } else {
      pending_txn_ = completer.ToAsync();
    }*/
    port_status_t status;
    if (last_observed_.has_value()) {
      status = {.flags = last_observed_.value().flags, .mtu = last_observed_.value().mtu};
    } else {
      status = {.flags = 0, .mtu = 0};
    }
    callback(status);
  } else {
    port_status_t status;
    last_observed_ = queue_.erase(0);
    if (last_observed_.has_value()) {
      status = {.flags = last_observed_.value().flags, .mtu = last_observed_.value().mtu};
    } else {
      status = {.flags = 0, .mtu = 0};
    }
    callback(status);
  }
}

void StatusWatcher::PushStatus(const port_status_t& status) {
  fbl::AutoLock lock(&lock_);
  std::optional<PortStatus> tail;
  if (queue_.is_empty()) {
    tail = last_observed_;
  } else {
    tail = queue_.data()[queue_.size() - 1];
  }
  if (tail.has_value() && tail.value() == status) {
    // ignore if no change is observed
    return;
  }

  if (/*pending_txn_.has_value() &&*/ queue_.is_empty()) {
    /*WithWireStatus(
        [completer = std::move(std::exchange(pending_txn_, std::nullopt).value())](
            netdev::wire::PortStatus wire_status) mutable { completer.Reply(wire_status); },
        status.flags(), status.mtu());*/
    last_observed_ = PortStatus(status);
  } else {
    fbl::AllocChecker ac;
    queue_.push_back(PortStatus(status), &ac);
    ZX_ASSERT(ac.check());
    // limit the queue to max_queue_
    if (queue_.size() > max_queue_) {
      queue_.erase(0);
    }
  }
}

}  // namespace network::internal
