// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_TESTING_CPP_TEST_ENVIRONMENT_H_
#define LIB_DRIVER_TESTING_CPP_TEST_ENVIRONMENT_H_

#include <lib/driver/component/cpp/driver_base.h>

namespace fdf_testing {

// The |TestEnvironment| manages the mocked test environment that the driver being tested uses.
// It provides the server backing the driver's incoming namespace. This incoming namespace can
// be customized by the user through the |incoming_directory| method.
//
// # Thread safety
//
// This class is thread-unsafe. Instances must be managed and used from a synchronized dispatcher.
// See
// https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/thread-safe-async#synchronized-dispatcher
//
class TestEnvironment {
 public:
  explicit TestEnvironment(async_dispatcher_t* dispatcher)
      : dispatcher_(dispatcher),
        incoming_directory_server_(dispatcher_),
        checker_(dispatcher_, kTestEnvironmentThreadSafetyDescription) {}

  // Get the component::OutgoingDirectory that backs the driver's incoming namespace.
  const component::OutgoingDirectory& incoming_directory() {
    std::lock_guard guard(checker_);
    return incoming_directory_server_;
  }

  zx::result<> Initialize(fidl::ServerEnd<fuchsia_io::Directory> incoming_directory_server_end);

 private:
  static const char kTestEnvironmentThreadSafetyDescription[];
  async_dispatcher_t* dispatcher_;
  component::OutgoingDirectory incoming_directory_server_;
  async::synchronization_checker checker_;
};

}  // namespace fdf_testing

#endif  // LIB_DRIVER_TESTING_CPP_TEST_ENVIRONMENT_H_
