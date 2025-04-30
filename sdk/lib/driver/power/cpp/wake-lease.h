// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_POWER_CPP_WAKE_LEASE_H_
#define LIB_DRIVER_POWER_CPP_WAKE_LEASE_H_

#include <fidl/fuchsia.power.system/cpp/wire.h>
#include <lib/async/cpp/task.h>
#include <lib/inspect/cpp/vmo/types.h>
#include <lib/zx/eventpair.h>
#include <lib/zx/result.h>

#include <string>

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)

namespace fdf_power {

// Wrapper around usage of fuchsia.power.system/ActivityGovernor.AcquireWakeLease. The wrapper
// reduces wake lease creation by allow callers to set timeout after which to drop the lease and
// provides mechanisms to extend that timeout.
class WakeLease : public fidl::WireServer<fuchsia_power_system::ActivityGovernorListener> {
 public:
  // If |log| is set to true, logs will be emitted when acquiring leases and when lease times out.
  // An invalid |sag_client| will result in silently disabling wake lease acquisition.
  WakeLease(async_dispatcher_t* dispatcher, std::string_view lease_name,
            fidl::ClientEnd<fuchsia_power_system::ActivityGovernor> sag_client,
            inspect::Node* parent_node = nullptr, bool log = false);

  // Ensure that the system stays awake until the timeout is reached. To accomplish this we either:
  //   * obtain a wake lease immediately if the system is currently suspended
  //   * obtain a wake lease in the future if the system begins suspension before the timeout is
  //     reached
  // If a wake lease is acquired, it is dropped when the timeout is reached. The timeout may be
  // extended by additional calls to `HandleInterrupt`. If the system is not suspended and does not
  // attempt suspension during the timeout period, no wake lease is obtained.
  //
  // Ideally this is only called after we wake up due to an interrupt. Note that a duration is
  // taken because the deadline is computed once the lease is acquired, rather than at the point
  // this method is called.
  bool HandleInterrupt(zx::duration timeout);

  // Acquire a wake lease and automatically drop it after the specified timeout. If a lease was
  // still held from an earlier invocation, it will be extended until the new timeout.
  // Note that a duration is taken because the deadline is computed once the lease is acquired,
  // rather than at the point this method is called.
  bool AcquireWakeLease(zx::duration timeout);

  // Provide a wake lease which will be dropped either:
  //   * immediately if there is already a wake lease with a later deadline
  //   * at the specified deadline
  // In the latter case any previous lease held by this object is dropped immediately.
  void DepositWakeLease(zx::eventpair wake_lease, zx::time timeout_deadline);

  // Cancel timeout and take the wake lease. Returns ZX_ERR_BAD_HANDLE if we don't currently have a
  // wake lease.
  zx::result<zx::eventpair> TakeWakeLease();

  // Get a duplicate of the stored wake lease. Returns ZX_ERR_BAD_HANDLE if we don't currently have
  // a wake lease.
  zx::result<zx::eventpair> GetWakeLeaseCopy();

  // fuchsia.power.system/ActivityGovernorListener implementation. This is used to avoid creating
  // wake leases in cases where the system is resumed and `HandleInterrupt` is called.
  void OnResume(OnResumeCompleter::Sync& completer) override;
  void OnSuspendStarted(OnSuspendStartedCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_power_system::ActivityGovernorListener> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  void SetSuspended(bool suspended);

  // Get the time our next timeout will occur. If there is no currently active
  // timeout this will be ZX_TIME_INFINITE.
  zx_time_t GetNextTimeout();

  // Returns `true` if the instance thinks the system is currently resumed.
  // This value may be wrong if the instance has not observed any system state
  // changes since it was created.
  bool IsResumed() const { return !system_suspended_; }

 private:
  void HandleTimeout();

  void ResetSagClient();

  async_dispatcher_t* dispatcher_;
  std::string lease_name_;
  bool log_;
  fidl::WireSyncClient<fuchsia_power_system::ActivityGovernor> sag_client_;
  std::optional<fidl::ServerBinding<fuchsia_power_system::ActivityGovernorListener>>
      listener_binding_;
  bool system_suspended_ = true;
  // Time before which we should take a wake lease if we are informed of
  // suspension.
  zx::time prevent_sleep_before_ = zx::time::infinite_past();

  async::TaskClosureMethod<WakeLease, &WakeLease::HandleTimeout> lease_task_{this};
  zx::eventpair lease_;

  inspect::UintProperty total_lease_acquisitions_;
  inspect::BoolProperty wake_lease_held_;
  inspect::BoolProperty wake_lease_grabbable_;
  inspect::UintProperty wake_lease_last_attempted_acquisition_timestamp_;
  inspect::UintProperty wake_lease_last_acquired_timestamp_;
  inspect::UintProperty wake_lease_last_refreshed_timestamp_;
};

// This is probably not the implementation you're looking for, consider using
// `SharedWakeLeaseProvider`. `ManagedWakeLease` may be appropriate if
// `SharedWakeLeaseProvider` does not fit your needs.
//
// ManagedWakeLease can be used to prevent the system from suspending. After a
// call to `Start()` returns the system will keep running until after `End()`
// is called or the instance is dropped. Users can use the same instance to
// perform multiple atomic operations, for example by calling `Start()` after
// `End()`.
//
// If doing multiple atomic operations, a single `ManagedWakeLease` can have
// performance advantages over using a WakeLease directly because the
// `ManagedWakeLease` monitors system state over its lifetime, allowing it to
// avoid certain operations vs a series of shorter-lived `WakeLease` instances.
class ManagedWakeLease {
 public:
  ManagedWakeLease(async_dispatcher_t* dispatcher, std::string_view name,
                   fidl::ClientEnd<fuchsia_power_system::ActivityGovernor> sag,
                   inspect::Node* parent_node = nullptr, bool log = false)
      : fdf_lease_(dispatcher, name, std::move(sag), parent_node, log) {}

  // Start an atomic operation. The system is guaranteed to stay running until
  // `End()` is called or this instance is dropped. Returns whether a wake
  // lease was actually taken as a result of this call.
  bool Start() { return fdf_lease_.HandleInterrupt(zx::duration::infinite()); }

  // Indicate that the operation is complete. The system may suspend after this
  // call is made. Calling `End()` makes sense in the case the caller wants to
  // use this instance to start a new atomic operation in the future. Returns
  // the wake lease currently currently held, if any.
  zx::result<zx::eventpair> End() { return fdf_lease_.TakeWakeLease(); }

  zx::result<zx::eventpair> GetWakeLeaseCopy() { return fdf_lease_.GetWakeLeaseCopy(); }

  bool IsResumed() { return fdf_lease_.IsResumed(); }

 private:
  WakeLease fdf_lease_;
};

// `SharedWakeLease` wraps a WakeLease. When `SharedWakeLease`
// destructs, the underlying system wake lease, if any currently held, is
// dropped.
class SharedWakeLease {
 public:
  explicit SharedWakeLease(const std::shared_ptr<WakeLease>& lease) : lease_(lease) {}
  zx::result<zx::eventpair> GetDuplicateLeaseHandle() { return lease_->GetWakeLeaseCopy(); }
  // Intended for testing, gets a shared pointer to the wrapped
  // fdf_power::WakeLease object.
  std::shared_ptr<WakeLease> GetWakeLease() { return lease_; }
  ~SharedWakeLease() { zx::result<zx::eventpair> obsolete_lease = lease_->TakeWakeLease(); }

 private:
  std::shared_ptr<WakeLease> lease_;
};

// This class is **not** threadsafe! `SharedWakeLeaseProvider` and
// `SharedWakeLease` pointers returned from `StartOperation` must be used
// on the same thread!
//
// Provides shared pointers to `SharedWakeLeases` which are effectively
// fdf_power::WakeLease objects. This provides RAII semantics to manage the
// lifecycle of the underlying wake lease such that for a given
// `SharedWakeLeaseProvider` there is _at_ _most_ one WakeLease in
// existence at any moment. It is worth noting that WakeLease only acquires a
// wake lease from the system if the system starts to suspend while the
// WakeLease is held.
//
// Using `SharedWakeLeaseProvider` can have performance advantages over
// using a WakeLease directly because the provider monitors system state
// over its lifetime, allowing it to avoid certain operations vs a series of
// shorter-lived `WakeLease` instances which would each only observe system
// state while they exist.
//
// The main difference between the shared pointer to a `SharedWakeLease`
// from `SharedWakeLeaseProvider` and an `ManagedWakeLease` is that with
// `SharedWakeLease` we get long-lived system state monitoring and
// ergonomic RAII management of the actual lease. With `ManagedWakeLease` you
// can get long-lived system state monitoring with `End` calls and sacrifice
// RAII ergonomics **or** get RAII ergonomics, but lose system state knowledge
// after the `ManagedWakeLease` is destroyed.
class SharedWakeLeaseProvider {
 public:
  SharedWakeLeaseProvider(async_dispatcher_t* dispatcher, std::string_view name,
                          fidl::ClientEnd<fuchsia_power_system::ActivityGovernor> sag,
                          inspect::Node* parent_node = nullptr, bool log = false)
      : fdf_lease_(
            std::make_shared<WakeLease>(dispatcher, name, std::move(sag), parent_node, log)) {}
  std::shared_ptr<SharedWakeLease> StartOperation() {
    // SharedWakeLeaseProvider works by holding and owning a
    // fdf_power::WakeLease and creating, but not owning, a SharedWakeLease.
    // The provider vends pointers to the SharedWakeLease, if it exists,
    // otherwise creating it. When the SharedWakeLease destructs, it drops the
    // actual wake lease held by the fdf_power::WakeLease that it wraps.

    // If we currently hold a SharedWakeLease, return a pointer to it,
    // otherwise create a new one with a reference to the wake lease we hold.
    std::shared_ptr<SharedWakeLease> op = atomic_op_.lock();
    if (!op) {
      // If we don't have a SharedWakeLease, call HandleInterrupt on the
      // WakeLease so that it is "active". Any previous SharedWakeLease
      // that destructed would have retrieved and dropped the actual wake lease
      // from the WakeLease object.
      fdf_lease_->HandleInterrupt(zx::duration::infinite());
      op = std::make_shared<SharedWakeLease>(fdf_lease_);
      atomic_op_ = op;
    }
    return op;
  }

 private:
  std::weak_ptr<SharedWakeLease> atomic_op_;
  std::shared_ptr<WakeLease> fdf_lease_;
};

}  // namespace fdf_power

#endif

#endif  // LIB_DRIVER_POWER_CPP_WAKE_LEASE_H_
