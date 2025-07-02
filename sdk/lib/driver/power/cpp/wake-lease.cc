// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/power/cpp/wake-lease.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/zx/clock.h>
#include <zircon/errors.h>
#include <zircon/rights.h>

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)

namespace fdf_power {

// This is probably not the implementation you're looking for, consider using
// `WakeLeaseProvider`. `ManualWakeLease` may be appropriate if
// `WakeLeaseProvider` does not fit your needs.
//
// ManualWakeLease can be used to prevent the system from suspending. After a
// call to `Start()` returns the system will keep running until after `End()`
// is called or the instance is dropped. Users can use the same instance to
// perform multiple atomic operations, for example by calling `Start()` after
// `End()`.
//
// If doing multiple atomic operations, a single `ManualWakeLease` can have
// performance advantages over using a WakeLease directly because the
// `ManualWakeLease` monitors system state over its lifetime, allowing it to
// avoid certain operations vs a series of shorter-lived `WakeLease` instances.
ManualWakeLease::ManualWakeLease(async_dispatcher_t* dispatcher, std::string_view name,
                                 fidl::ClientEnd<fuchsia_power_system::ActivityGovernor> sag,
                                 inspect::Node* parent_node, bool log)
    : lease_name_(name), log_(log) {
  if (sag) {
    sag_client_.Bind(std::move(sag));

    auto [client_end, server_end] =
        fidl::Endpoints<fuchsia_power_system::ActivityGovernorListener>::Create();
    fidl::Arena arena;
    fidl::WireResult result = sag_client_->RegisterListener(
        fuchsia_power_system::wire::ActivityGovernorRegisterListenerRequest::Builder(arena)
            .listener(std::move(client_end))
            .Build());
    if (!result.ok()) {
      if (log_) {
        fdf::warn("Failed to register for sag state listener: {}", result.error());
      }
      ResetSagClient();
    } else {
      listener_binding_.emplace(dispatcher, std::move(server_end), this,
                                [this](fidl::UnbindInfo unused) { ResetSagClient(); });
    }
  }

  if (parent_node) {
    total_lease_acquisitions_ = parent_node->CreateUint("Total Lease Acquisitions", 0);
    wake_lease_held_ = parent_node->CreateBool("Wake Lease Held", false);
    wake_lease_grabbable_ = parent_node->CreateBool("Wake Lease Grabbable", sag_client_.is_valid());
    wake_lease_last_attempted_acquisition_timestamp_ =
        parent_node->CreateUint("Wake Lease Last Attempted Acquisition Timestamp (ns)", 0);
    wake_lease_last_acquired_timestamp_ =
        parent_node->CreateUint("Wake Lease Last Acquired Timestamp (ns)", 0);
    wake_lease_last_refreshed_timestamp_ =
        parent_node->CreateUint("Wake Lease Last Refreshed Timestamp (ns)", 0);
  }
}

// Start an atomic operation. The system is guaranteed to stay running until
// `End()` is called or this instance is dropped. Returns whether a wake
// lease was actually taken as a result of this call. Note that if `Start`
// is called twice in succession, the first call *may* return true whereas
// the second call would may return false, assuming the lease from taken
// because of the first call is still held.
bool ManualWakeLease::Start(bool ignore_system_state) {
  // Why do we care about ignore_system_state here?
  // Because we might have previously called this method with `false` which
  // set active_ to true and as a result we might not have acquired a lease.
  // Now, if we are called with ignore_system_state true, we need to consider
  // the possibility we'll need to acquire an actual lease.
  if (active_ && !ignore_system_state) {
    return true;
  }

  active_ = true;
  bool lease_acquired = AcquireLease(ignore_system_state);

  // Set the suspension state to false because either we already had a lease
  // or we acquired one, in which case we aren't suspended or won't be soon.
  system_suspended_ = false;
  return lease_acquired;
}

// Indicate that the operation is complete. The system may suspend after this
// call is made. Calling `End()` makes sense in the case the caller wants to
// use this instance to start a new atomic operation in the future. Returns
// the wake lease currently currently held, if any.
zx::result<zx::eventpair> ManualWakeLease::End() {
  active_ = false;
  return TakeWakeLease();
}

// Stores the wake lease in the instance. The wake lease will be retained
// until `End` or `TakeWakeLease` is called.
void ManualWakeLease::DepositWakeLease(zx::eventpair wake_lease) {
  lease_ = std::move(wake_lease);
  active_ = true;

  wake_lease_last_refreshed_timestamp_.Set(zx::clock::get_monotonic().get());
  wake_lease_held_.Set(true);
}

// Consider whether End() is more appropriate for the use case.
//
// Returns ZX_ERR_BAD_HANDLE if we don't currently have a wake lease.
// IMPORTANT: This does not implicitly call `End()`, meaning that at the next
// system transition to suspend, this class will take a wake lease and cause
// the suspend to abort.
zx::result<zx::eventpair> ManualWakeLease::TakeWakeLease() {
  wake_lease_held_.Set(false);
  if (lease_) {
    return zx::ok(std::move(lease_));
  }
  return zx::error(ZX_ERR_BAD_HANDLE);
}

// Get a duplicate of the stored wake lease. Returns ZX_ERR_BAD_HANDLE if we don't currently have
// a wake lease.
zx::result<zx::eventpair> ManualWakeLease::GetWakeLeaseCopy() {
  if (!lease_.is_valid()) {
    return zx::error(ZX_ERR_BAD_HANDLE);
  }
  zx::eventpair copy;
  lease_.duplicate(ZX_RIGHT_SAME_RIGHTS, &copy);
  return zx::ok(std::move(copy));
}

// fuchsia.power.system/ActivityGovernorListener implementation. This is used to avoid creating
// wake leases in cases where the system is resumed and `HandleInterrupt` is called.
void ManualWakeLease::OnResume(OnResumeCompleter::Sync& completer) {
  SetSuspended(false);
  completer.Reply();
}

void ManualWakeLease::OnSuspendStarted(OnSuspendStartedCompleter::Sync& completer) {
  SetSuspended(true);
  if (lease_ && log_) {
    // This is a bit unexpected, since we have a lease, but maybe this
    // callback was queued before we acquired the lease?
    fdf::warn("OnSuspendStarted call while a wake lease is held.");
  }

  if (active_) {
    AcquireLease(false);
  }

  completer.Reply();
}

void ManualWakeLease::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_power_system::ActivityGovernorListener> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  if (log_) {
    fdf::warn("Encountered unexpected method: {}", metadata.method_ordinal);
  }
}

void ManualWakeLease::ResetSagClient() {
  sag_client_ = {};
  wake_lease_grabbable_.Set(false);
}

bool ManualWakeLease::AcquireLease(bool ignore_system_state) {
  if (lease_) {
    return true;
  }

  if (!sag_client_) {
    return false;
  }

  if (!ignore_system_state && !system_suspended_) {
    return true;
  }

  wake_lease_last_attempted_acquisition_timestamp_.Set(zx::clock::get_monotonic().get());
  auto result_lease = sag_client_->AcquireWakeLease(fidl::StringView::FromExternal(lease_name_));
  if (!result_lease.ok()) {
    if (log_) {
      fdf::warn(
          "Failed to call AcquireWakeLease(), system may incorrectly enter "
          "suspend: {}. Will not attempt again.",
          result_lease.error());
    }
    ResetSagClient();
    return false;
  }
  if (result_lease->is_error()) {
    if (log_) {
      fdf::warn(
          "System activity governor failed to acquire wake lease, "
          "system may incorrectly enter suspend: {}. Will not attempt again.",
          static_cast<uint32_t>(result_lease->error_value()));
    }
    ResetSagClient();
    return false;
  }

  lease_ = std::move(result_lease->value()->token);
  if (log_) {
    fdf::info("Created a wake lease due to new request.");
  }
  auto now = zx::clock::get_monotonic().get();
  wake_lease_last_acquired_timestamp_.Set(now);
  wake_lease_last_refreshed_timestamp_.Set(now);
  total_lease_acquisitions_.Add(1);
  wake_lease_held_.Set(true);

  return true;
}

TimeoutWakeLease::TimeoutWakeLease(
    async_dispatcher_t* dispatcher, std::string_view lease_name,
    fidl::ClientEnd<fuchsia_power_system::ActivityGovernor> sag_client, inspect::Node* parent_node,
    bool log)
    : lease_(dispatcher, lease_name, std::move(sag_client), parent_node, log),
      dispatcher_(dispatcher),
      log_(log),
      lease_name_(lease_name) {}

bool TimeoutWakeLease::HandleInterrupt(zx::duration timeout) {
  bool acquired = lease_.Start();
  this->ResetTimeout(timeout);
  return acquired;
}

void TimeoutWakeLease::ResetTimeout(zx::time timeout) {
  lease_task_.Cancel();
  lease_task_.PostForTime(dispatcher_, timeout);
}

void TimeoutWakeLease::ResetTimeout(zx::duration timeout) {
  lease_task_.Cancel();
  // Post a task to drop the lease if we have one, but not if the timeout
  // is infinity, since that time should never come.
  if (timeout < zx::duration::infinite()) {
    lease_task_.PostDelayed(dispatcher_, timeout);
  }
}

zx_time_t TimeoutWakeLease::GetNextTimeout() { return lease_task_.last_deadline().get(); }

bool TimeoutWakeLease::AcquireWakeLease(zx::duration timeout) {
  bool acquired = lease_.Start(true);
  this->ResetTimeout(timeout);
  return acquired;
}

void TimeoutWakeLease::DepositWakeLease(zx::eventpair wake_lease, zx::time timeout_deadline) {
  // If the current deadline has been set and the new one is sooner,
  // just return.
  zx::time next_timeout = lease_task_.last_deadline();
  if (next_timeout != zx::time::infinite() && timeout_deadline < lease_task_.last_deadline()) {
    return;
  }

  lease_task_.Cancel();
  lease_.DepositWakeLease(std::move(wake_lease));
  lease_task_.PostForTime(dispatcher_, timeout_deadline);
}

zx::result<zx::eventpair> TimeoutWakeLease::TakeWakeLease() {
  lease_task_.Cancel();
  return lease_.End();
}

zx::result<zx::eventpair> TimeoutWakeLease::GetWakeLeaseCopy() { return lease_.GetWakeLeaseCopy(); }

void TimeoutWakeLease::HandleTimeout() {
  if (log_) {
    fdf::info("Dropping the wake lease due to not receiving any wake events.");
  }
  auto discard = lease_.End();
}

}  // namespace fdf_power

#endif
