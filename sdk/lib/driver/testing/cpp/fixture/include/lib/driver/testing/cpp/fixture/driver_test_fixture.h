// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_TESTING_CPP_FIXTURE_DRIVER_TEST_FIXTURE_H_
#define LIB_DRIVER_TESTING_CPP_FIXTURE_DRIVER_TEST_FIXTURE_H_

#include <lib/driver/testing/cpp/fixture/driver_test.h>

// This library provides a layer on top the |driver_test.h| RAII-style classes. These classes
// can be inherited by test fixtures, and provide passthrough functions that can be used by the
// inheriting fixture class and tests rather than having to use a field. It provides
// |BackgroundDriverTestFixture| and |ForegroundDriverTestFixture|.
//
// The choice between foreground and background driver tests lies in how the test plans to
// communicate with the driver-under-test. If the test will be calling public methods on the driver
// a lot, the foreground driver test should be chosen. If the test will be calling through the
// driver's exposed FIDL more often, then the background driver test should be chosen.
//
// Both test kinds can be configured through a struct/class provided through a template parameter.
// This configuration must define two types through using statements:
//
//   DriverType: The type of the driver under test.
//     If using a test-specific driver, ensure the DriverType contains a static function
//     in the format below. This registration is what the test uses to manage the driver.
//       `static DriverRegistration GetDriverRegistration()`
//     If the driver type is not known to the test, or is not needed by the test, then use the
//     |EmptyDriverType| class as a placeholder.
//
//   EnvironmentType: A class that contains environment dependencies of the driver-under-test.
//     This must inherit from the |Environment| base class and provide the |Serve| method.
//     It must also contain a default constructor.
//     The environment will live on a background dispatcher during the lifetime of the test.
namespace fdf_testing {

// Background driver tests have the driver-under-test executing on a background driver dispatcher.
// This allows for tests to use sync FIDL clients directly from their main test thread.
// This is good for unit tests that more heavily exercise the driver-under-test through its exposed
// FIDL services, rather than its public methods.
//
// The test can run tasks on the driver context using the |RunInDriverContext()| methods, but sync
// client tasks can be run directly on the main test thread.
template <typename Configuration>
class BackgroundDriverTestFixture {
  using DriverType = typename Configuration::DriverType;
  using EnvironmentType = typename Configuration::EnvironmentType;

 public:
  // See |DriverTestCommon::runtime()|.
  fdf_testing::DriverRuntime& runtime() { return inner_.runtime(); }

  // See |DriverTestCommon::Connect()|.
  template <typename ServiceMember,
            typename = std::enable_if_t<fidl::IsServiceMemberV<ServiceMember>>>
  zx::result<fidl::internal::ClientEndType<typename ServiceMember::ProtocolType>> Connect(
      std::string_view instance = component::kDefaultInstance) {
    return inner_.template Connect<ServiceMember>(instance);
  }

  // See |DriverTestCommon::RunInEnvironmentTypeContext()|.
  template <typename T>
  T RunInEnvironmentTypeContext(fit::callback<T(EnvironmentType&)> task) {
    return inner_.template RunInEnvironmentTypeContext<T>(std::move(task));
  }

  // See |DriverTestCommon::RunInEnvironmentTypeContext()|.
  void RunInEnvironmentTypeContext(fit::callback<void(EnvironmentType&)> task) {
    inner_.RunInEnvironmentTypeContext(std::move(task));
  }

  // See |DriverTestCommon::RunInNodeContext()|.
  template <typename T>
  T RunInNodeContext(fit::callback<T(fdf_testing::TestNode&)> task) {
    return inner_.template RunInNodeContext<T>(std::move(task));
  }

  // See |DriverTestCommon::RunInNodeContext()|.
  void RunInNodeContext(fit::callback<void(fdf_testing::TestNode&)> task) {
    return inner_.RunInNodeContext(std::move(task));
  }

  // See |DriverTestCommon::ConnectThroughDevfs()|.
  template <typename ProtocolType, typename = std::enable_if_t<fidl::IsProtocolV<ProtocolType>>>
  zx::result<fidl::ClientEnd<ProtocolType>> ConnectThroughDevfs(std::string_view devfs_node_name) {
    return inner_.template ConnectThroughDevfs<ProtocolType>(devfs_node_name);
  }

  // See |DriverTestCommon::ConnectThroughDevfs()|.
  template <typename ProtocolType, typename = std::enable_if_t<fidl::IsProtocolV<ProtocolType>>>
  zx::result<fidl::ClientEnd<ProtocolType>> ConnectThroughDevfs(
      std::vector<std::string> devfs_node_name_path) {
    return inner_.template ConnectThroughDevfs<ProtocolType>(std::move(devfs_node_name_path));
  }

  // See |DriverTestCommon::StartDriver()|.
  zx::result<> StartDriver() { return inner_.StartDriver(); }

  // See |DriverTestCommon::StartDriverCustomized()|.
  zx::result<> StartDriverCustomized(fit::callback<void(fdf::DriverStartArgs&)> args_modifier) {
    return inner_.StartDriverCustomized(std::move(args_modifier));
  }

  // See |DriverTestCommon::StopDriver()|.
  zx::result<> StopDriver() { return inner_.StopDriver(); }

  // See |DriverTestCommon::ShutdownAndDestroyDriver()|.
  void ShutdownAndDestroyDriver() { inner_.ShutdownAndDestroyDriver(); }

  // See |BackgroundDriverTest::RunInDriverContext()|.
  template <typename T>
  T RunInDriverContext(fit::callback<T(DriverType&)> task) {
    return inner_.template RunInDriverContext<T>(std::move(task));
  }

  // See |BackgroundDriverTest::RunInDriverContext()|.
  void RunInDriverContext(fit::callback<void(DriverType&)> task) {
    inner_.RunInDriverContext(std::move(task));
  }

 private:
  BackgroundDriverTest<Configuration> inner_;
};

// Foreground driver tests have the driver-under-test executing on the foreground (main) test
// thread. This allows for the test to directly reach into the driver to call methods on it.
// This is good for unit tests that more heavily test a driver through its public methods,
// rather than its exposed FIDL services.
//
// The test can access the driver under test using the |driver()| method and directly make calls
// into it, but sync client tasks must go through |RunInBackground()|.
template <typename Configuration>
class ForegroundDriverTestFixture {
  using DriverType = typename Configuration::DriverType;
  using EnvironmentType = typename Configuration::EnvironmentType;

 public:
  // See |DriverTestCommon::runtime()|.
  fdf_testing::DriverRuntime& runtime() { return inner_.runtime(); }

  // See |DriverTestCommon::Connect()|.
  template <typename ServiceMember,
            typename = std::enable_if_t<fidl::IsServiceMemberV<ServiceMember>>>
  zx::result<fidl::internal::ClientEndType<typename ServiceMember::ProtocolType>> Connect(
      std::string_view instance = component::kDefaultInstance) {
    return inner_.template Connect<ServiceMember>(instance);
  }

  // See |DriverTestCommon::RunInEnvironmentTypeContext()|.
  template <typename T>
  T RunInEnvironmentTypeContext(fit::callback<T(EnvironmentType&)> task) {
    return inner_.template RunInEnvironmentTypeContext<T>(std::move(task));
  }

  // See |DriverTestCommon::RunInEnvironmentTypeContext()|.
  void RunInEnvironmentTypeContext(fit::callback<void(EnvironmentType&)> task) {
    inner_.RunInEnvironmentTypeContext(std::move(task));
  }

  // See |DriverTestCommon::RunInNodeContext()|.
  template <typename T>
  T RunInNodeContext(fit::callback<T(fdf_testing::TestNode&)> task) {
    return inner_.template RunInNodeContext<T>(std::move(task));
  }

  // See |DriverTestCommon::RunInNodeContext()|.
  void RunInNodeContext(fit::callback<void(fdf_testing::TestNode&)> task) {
    return inner_.RunInNodeContext(std::move(task));
  }

  // See |DriverTestCommon::ConnectThroughDevfs()|.
  template <typename ProtocolType, typename = std::enable_if_t<fidl::IsProtocolV<ProtocolType>>>
  zx::result<fidl::ClientEnd<ProtocolType>> ConnectThroughDevfs(std::string_view devfs_node_name) {
    return inner_.template ConnectThroughDevfs<ProtocolType>(devfs_node_name);
  }

  // See |DriverTestCommon::ConnectThroughDevfs()|.
  template <typename ProtocolType, typename = std::enable_if_t<fidl::IsProtocolV<ProtocolType>>>
  zx::result<fidl::ClientEnd<ProtocolType>> ConnectThroughDevfs(
      std::vector<std::string> devfs_node_name_path) {
    return inner_.ConnectThroughDevfs(std::move(devfs_node_name_path));
  }

  // See |DriverTestCommon::StartDriver()|.
  zx::result<> StartDriver() { return inner_.StartDriver(); }

  // See |DriverTestCommon::StartDriverCustomized()|.
  zx::result<> StartDriverCustomized(fit::callback<void(fdf::DriverStartArgs&)> args_modifier) {
    return inner_.StartDriverCustomized(std::move(args_modifier));
  }

  // See |DriverTestCommon::StopDriver()|.
  zx::result<> StopDriver() { return inner_.StopDriver(); }

  // See |DriverTestCommon::ShutdownAndDestroyDriver()|.
  void ShutdownAndDestroyDriver() { inner_.ShutdownAndDestroyDriver(); }

  // See |ForegroundDriverTest::RunInBackground()|.
  template <typename T>
  zx::result<T> RunInBackground(fit::callback<T()> task) {
    return inner_.template RunInBackground<T>(std::move(task));
  }

  // See |ForegroundDriverTest::RunInBackground()|.
  zx::result<> RunInBackground(fit::callback<void()> task) {
    return inner_.RunInBackground(std::move(task));
  }

  // See |ForegroundDriverTest::driver()|.
  DriverType* driver() { return inner_.driver(); }

 private:
  ForegroundDriverTest<Configuration> inner_;
};

}  // namespace fdf_testing

#endif  // LIB_DRIVER_TESTING_CPP_FIXTURE_DRIVER_TEST_FIXTURE_H_
