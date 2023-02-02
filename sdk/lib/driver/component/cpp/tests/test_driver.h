// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPONENT_CPP_TESTS_TEST_DRIVER_H_
#define LIB_DRIVER_COMPONENT_CPP_TESTS_TEST_DRIVER_H_

#include <lib/driver/component/cpp/driver_cpp.h>

class TestDriver : public fdf::DriverBase {
 public:
  TestDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase("test_driver", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override;

  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  void CreateChildNodeSync();

  void CreateChildNodeAsync();

  bool async_added_child() const { return async_added_child_; }
  bool sync_added_child() const { return sync_added_child_; }

 private:
  fidl::WireClient<fuchsia_driver_framework::Node> node_client_;
  bool async_added_child_ = false;
  bool sync_added_child_ = false;
};

#endif  // LIB_DRIVER_COMPONENT_CPP_TESTS_TEST_DRIVER_H_
