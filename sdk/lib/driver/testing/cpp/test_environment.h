// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_TESTING_CPP_TEST_ENVIRONMENT_H_
#define LIB_DRIVER_TESTING_CPP_TEST_ENVIRONMENT_H_

#include <lib/driver/component/cpp/driver_base.h>

namespace fdf_testing {

// The |AsyncTestEnvironment| manages the mocked test environment that the driver being tested uses.
// It provides the server backing the driver's incoming namespace. This incoming namespace can
// be customized by the user through the |incoming_directory| method. The |AsyncTestEnvironment|
// uses a component::OutgoingDirectory which cannot support driver transport. It must not use a
// driver runtime dispatcher.
//
// # Thread safety
//
// This class is thread-unsafe. Instances must be managed and used from a synchronized dispatcher.
// See
// https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/thread-safe-async#synchronized-dispatcher
//
// ```
// async::Loop test_env_dispatcher_{&kAsyncLoopConfigNoAttachToCurrentThread};
//
// async_patterns::TestDispatcherBound<fdf_testing::AsyncTestEnvironment> test_environment_{
//      test_env_dispatcher_.dispatcher(), std::in_place};
// ```
//
class AsyncTestEnvironment {
 public:
  explicit AsyncTestEnvironment(async_dispatcher_t* dispatcher = nullptr);

  // Get the component::OutgoingDirectory that backs the driver's incoming namespace.
  const component::OutgoingDirectory& incoming_directory() const {
    std::lock_guard guard(checker_);
    return incoming_directory_server_;
  }

  component::OutgoingDirectory& incoming_directory() {
    std::lock_guard guard(checker_);
    return incoming_directory_server_;
  }

  zx::result<> Initialize(fidl::ServerEnd<fuchsia_io::Directory> incoming_directory_server_end);

 private:
  static const char kAsyncTestEnvironmentThreadSafetyDescription[];
  async_dispatcher_t* dispatcher_;
  component::OutgoingDirectory incoming_directory_server_;
  async::synchronization_checker checker_;
};

// The |TestEnvironment| manages the mocked test environment that the driver being tested uses.
// It provides the server backing the driver's incoming namespace. This incoming namespace can
// be customized by the user through the |incoming_directory| method. The |TestEnvironment|
// uses a fdf::OutgoingDirectory which can support driver transport. Therefore it must be used
// with an fdf_dispatcher, which can be gotten using |fdf::TestSynchronizedDispatcher|.
//
// # Thread safety
//
// This class is thread-unsafe. Instances must be managed and used from a synchronized dispatcher.
// See
// https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/thread-safe-async#synchronized-dispatcher
//
// If the dispatcher is not the default dispatcher of the main thread, the suggestion is to wrap
// this inside of an |async_patterns::TestDispatcherBound|. Example below:
//
// ```
// fdf::TestSynchronizedDispatcher test_env_dispatcher_{fdf::kDispatcherManaged};
//
// async_patterns::TestDispatcherBound<fdf_testing::TestEnvironment> test_environment_{
//      test_env_dispatcher_.dispatcher(), std::in_place};
// ```
//
class TestEnvironment {
 public:
  explicit TestEnvironment(fdf_dispatcher_t* dispatcher = nullptr);

  // Get the component::OutgoingDirectory that backs the driver's incoming namespace.
  fdf::OutgoingDirectory& incoming_directory() {
    std::lock_guard guard(checker_);
    return incoming_directory_server_;
  }

  const fdf::OutgoingDirectory& incoming_directory() const {
    std::lock_guard guard(checker_);
    return incoming_directory_server_;
  }

  zx::result<> Initialize(fidl::ServerEnd<fuchsia_io::Directory> incoming_directory_server_end);

 private:
  static const char kTestEnvironmentThreadSafetyDescription[];
  fdf_dispatcher_t* dispatcher_;
  fdf::OutgoingDirectory incoming_directory_server_;
  async::synchronization_checker checker_;
};

}  // namespace fdf_testing

#endif  // LIB_DRIVER_TESTING_CPP_TEST_ENVIRONMENT_H_
