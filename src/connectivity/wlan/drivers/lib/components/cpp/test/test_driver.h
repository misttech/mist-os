// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_LIB_COMPONENTS_CPP_TEST_TEST_DRIVER_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_LIB_COMPONENTS_CPP_TEST_TEST_DRIVER_H_

#include <lib/driver/component/cpp/driver_base.h>

namespace wlan::drivers::components::test {

// Since this is a library for use by drivers it doesn't contain a driver of its own. Create a
// driver for the tests to use to interact with.
class TestDriver : public fdf::DriverBase {
 public:
  class StopHandler {
   public:
    virtual ~StopHandler() = default;

    virtual void PrepareStop(fdf::PrepareStopCompleter completer) = 0;
    virtual void Stop() {}
  };

  TestDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher);

  void Start(fdf::StartCompleter completer) override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;
  void Stop() override;

  void AssignStopHandler(StopHandler* stop_handler) { stop_handler_ = stop_handler; }
  fidl::ClientEnd<fuchsia_driver_framework::Node>& node() { return DriverBase::node(); }
  std::shared_ptr<fdf::OutgoingDirectory>& outgoing() { return DriverBase::outgoing(); }

 private:
  StopHandler* stop_handler_ = nullptr;
};

}  // namespace wlan::drivers::components::test

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_LIB_COMPONENTS_CPP_TEST_TEST_DRIVER_H_
