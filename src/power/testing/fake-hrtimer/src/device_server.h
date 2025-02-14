// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_POWER_TESTING_FAKE_HRTIMER_SRC_DEVICE_SERVER_H_
#define SRC_POWER_TESTING_FAKE_HRTIMER_SRC_DEVICE_SERVER_H_

#include <fidl/fuchsia.hardware.hrtimer/cpp/fidl.h>

#include "lib/fidl/cpp/channel.h"

namespace fake_hrtimer {

// Protocol served to client components over devfs.
class DeviceServer : public fidl::Server<fuchsia_hardware_hrtimer::Device> {
 public:
  struct HrTimerState {
    // If set, there is an ongoing timer.  New requests without a preceding
    // cancel are not allowed.
    bool active = false;

    // If set, a cancel is requested. It is up to the handler to ack the completer.
    std::optional<std::function<void()>> completer;
  };

  explicit DeviceServer();
  void Start(StartRequest& request, StartCompleter::Sync& completer) override;
  void Stop(StopRequest& request, StopCompleter::Sync& completer) override;
  void GetTicksLeft(GetTicksLeftRequest& request, GetTicksLeftCompleter::Sync& completer) override;
  void SetEvent(SetEventRequest& request, SetEventCompleter::Sync& completer) override;
  void StartAndWait(StartAndWaitRequest& request, StartAndWaitCompleter::Sync& completer) override;
  void StartAndWait2(StartAndWait2Request& request,
                     StartAndWait2Completer::Sync& completer) override;
  void GetProperties(GetPropertiesCompleter::Sync& completer) override;
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_hardware_hrtimer::Device> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override;
  void Serve(async_dispatcher_t* dispatcher,
             fidl::ServerEnd<fuchsia_hardware_hrtimer::Device> server);

 private:
  // Adds code to process double programming of the hrtimer.
  //
  // Types:
  // - R is the response type, for example fuchsia_hardware_hrtimer::DeviceStartAndWaitResponse.
  // - C is the completer type, for example SetAndWaitCompleter.
  //
  // Returns:
  // - a value that should be returned right away if set; or continue execution if unset.
  template <typename R, typename C>
  std::optional<zx::result<R>> HandleBadState(C::Sync& completer) {
    std::lock_guard<std::mutex> guard(lock_);
    // Can not schedule a new timer without removing the current one.
    // Hardware returns bad state here, so the fake does it too.
    if (state_.active) {
      R response;
      completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kBadState));
      // Unblock the next scheduled timer. Hardware usually does not like this and
      // has to be recovered manually. But it probably will work out here.
      state_.active = false;
      state_.completer.value()();
      return zx::ok(std::move(response));
    }

    state_.active = true;
    return std::nullopt;
  }

  // Adds code to handle timer cancelation.
  //
  // See HandleBadState above for details
  template <typename R, typename C>
  std::optional<zx::result<R>> HandleCanceled(C::Sync& completer) {
    // Since we can not easily stop the thread from sleeping, we do something else
    // if a Stop intervenes. We wait for the timer to expire, then respond
    // with a "canceled" on Stop's completer.
    //
    // This is not "optimal" in the sense that the
    // wait is not interrupted as it should be, but is still the correct response.
    std::lock_guard<std::mutex> guard(lock_);
    if (state_.completer) {
      R response;
      completer.Reply(zx::error(fuchsia_hardware_hrtimer::DriverError::kCanceled));

      // Return from a pending Stop.
      state_.completer.value()();
      return zx::ok(std::move(response));
    }
    state_.active = false;
    return std::nullopt;
  }

  std::optional<zx::result<fuchsia_hardware_hrtimer::DeviceStartAndWait2Response>> HandleBadState2(
      StartAndWait2Completer::Sync& completer);
  std::optional<zx::result<fuchsia_hardware_hrtimer::DeviceStartAndWait2Response>> HandleCanceled2(
      StartAndWait2Completer::Sync& completer);

  fidl::ServerBindingGroup<fuchsia_hardware_hrtimer::Device> bindings_;
  std::optional<zx::event> event_;
  std::optional<fidl::ClientEnd<fuchsia_power_broker::ElementControl>> element_control_client_;
  std::optional<fidl::SyncClient<fuchsia_power_broker::CurrentLevel>> current_level_;
  std::optional<fidl::SyncClient<fuchsia_power_broker::RequiredLevel>> required_level_;
  std::optional<fidl::SyncClient<fuchsia_power_broker::Lessor>> lessor_;

  mutable std::mutex lock_;
  HrTimerState state_ __TA_GUARDED(lock_);
};

}  // namespace fake_hrtimer

#endif  // SRC_POWER_TESTING_FAKE_HRTIMER_SRC_DEVICE_SERVER_H_
