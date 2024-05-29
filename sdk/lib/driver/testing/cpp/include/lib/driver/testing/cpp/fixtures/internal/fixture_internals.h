// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_TESTING_CPP_FIXTURES_INTERNAL_FIXTURE_INTERNALS_H_
#define LIB_DRIVER_TESTING_CPP_FIXTURES_INTERNAL_FIXTURE_INTERNALS_H_

#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>

#include <type_traits>

namespace fdf_testing {

// Forward declare.
class Environment;

namespace internal {

template <typename EnvironmentType>
class EnvWrapper {
 public:
  fdf::DriverStartArgs Init() {
    zx::result start_args = node_server_.CreateStartArgsAndServe();
    ZX_ASSERT_MSG(start_args.is_ok(), "Failed to CreateStartArgsAndServe: %s.",
                  start_args.status_string());

    zx::result result =
        test_environment_.Initialize(std::move(start_args->incoming_directory_server));
    ZX_ASSERT_MSG(result.is_ok(), "Failed to Initialize the test_environment: %s.",
                  result.status_string());
    outgoing_client_ = std::move(start_args->outgoing_directory_client);

    result = user_env_.Serve(test_environment_.incoming_directory());
    ZX_ASSERT_MSG(result.is_ok(), "Failed to Serve the user's Environment: %s",
                  result.status_string());

    return std::move(start_args->start_args);
  }

  fidl::ClientEnd<fuchsia_io::Directory> TakeOutgoingClient() {
    ZX_ASSERT_MSG(outgoing_client_.is_valid(), "Cannot call TakeOutgoingClient more than once.");
    return std::move(outgoing_client_);
  }

  fdf_testing::TestNode& node_server() { return node_server_; }

  EnvironmentType& user_env() { return user_env_; }

 private:
  fdf_testing::TestNode node_server_{"root"};
  fdf_testing::TestEnvironment test_environment_;
  fidl::ClientEnd<fuchsia_io::Directory> outgoing_client_;

  // User env should be the last field as it could contain references to the test_environment_.
  EnvironmentType user_env_;
};

template <typename DriverType, bool DriverOnForeground>
class DriverWrapper {
  template <typename T, typename = void>
  struct HasGetDriverRegistration : public ::std::false_type {};
  template <typename T>
  struct HasGetDriverRegistration<T, std::void_t<decltype(T::GetDriverRegistration)>>
      : public std::true_type{};
  template <typename T>
  constexpr static auto HasGetDriverRegistrationV = HasGetDriverRegistration<T>::value;

 public:
  // The runtime must outlive this class.
  explicit DriverWrapper(fdf_testing::DriverRuntime* runtime) : runtime_(runtime) {
    DriverRegistration driver_registration_symbol;
    if constexpr (HasGetDriverRegistrationV<DriverType>) {
      driver_registration_symbol = DriverType::GetDriverRegistration();
    } else {
      driver_registration_symbol = __fuchsia_driver_registration__;
    }

    if constexpr (DriverOnForeground) {
      dut_dispatcher_ = fdf::Dispatcher::GetCurrent();
      dut_.template emplace<fdf_testing::DriverUnderTest<DriverType>>(driver_registration_symbol);
    } else {
      dut_dispatcher_ = fdf::UnownedDispatcher(runtime->StartBackgroundDispatcher()->get());
      dut_.template emplace<
          async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<DriverType>>>(
          dut_dispatcher_->async_dispatcher(), std::in_place, driver_registration_symbol);
    }
  }

  zx::result<> Start(fdf::DriverStartArgs start_args) {
    if constexpr (DriverOnForeground) {
      auto* fg_driver = std::get_if<fdf_testing::DriverUnderTest<DriverType>>(&dut_);
      ZX_ASSERT_MSG(fg_driver, "Could not get the foreground driver variant.");

      return runtime_->RunToCompletion(fg_driver->Start(std::move(start_args)));
    }

    auto* bg_driver =
        std::get_if<async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<DriverType>>>(
            &dut_);
    ZX_ASSERT_MSG(bg_driver, "Could not get the background driver variant.");

    zx::result start_result = runtime_->RunToCompletion(bg_driver->SyncCall(
        &fdf_testing::DriverUnderTest<DriverType>::Start, std::move(start_args)));
    return start_result;
  }
  zx::result<> PrepareStop() {
    if constexpr (DriverOnForeground) {
      auto* fg_driver = std::get_if<fdf_testing::DriverUnderTest<DriverType>>(&dut_);
      ZX_ASSERT_MSG(fg_driver, "Could not get the foreground driver variant.");

      return runtime_->RunToCompletion(fg_driver->PrepareStop());
    }

    auto* bg_driver =
        std::get_if<async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<DriverType>>>(
            &dut_);
    ZX_ASSERT_MSG(bg_driver, "Could not get the background driver variant.");

    zx::result start_result = runtime_->RunToCompletion(
        bg_driver->SyncCall(&fdf_testing::DriverUnderTest<DriverType>::PrepareStop));
    return start_result;
  }

  fdf::UnownedDispatcher GetDispatcher() { return dut_dispatcher_->borrow(); }

  template <typename T, bool F = DriverOnForeground, typename = std::enable_if_t<!F>>
  T RunInDriverContext(fit::callback<T(DriverType&)> task) {
    static_assert(F == DriverOnForeground);

    auto* bg_driver =
        std::get_if<async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<DriverType>>>(
            &dut_);
    ZX_ASSERT_MSG(bg_driver, "Could not get the background driver variant.");

    return bg_driver->SyncCall(
        [driver_task = std::move(task)](fdf_testing::DriverUnderTest<DriverType>* dut_ptr) mutable {
          return driver_task(***dut_ptr);
        });
  }

  template <bool F = DriverOnForeground, typename = std::enable_if_t<!F>>
  void RunInDriverContext(fit::callback<void(DriverType&)> task) {
    static_assert(F == DriverOnForeground);

    auto* bg_driver =
        std::get_if<async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<DriverType>>>(
            &dut_);
    ZX_ASSERT_MSG(bg_driver, "Could not get the background driver variant.");

    bg_driver->SyncCall(
        [driver_task = std::move(task)](fdf_testing::DriverUnderTest<DriverType>* dut_ptr) mutable {
          driver_task(***dut_ptr);
        });
  }

  template <bool F = DriverOnForeground, typename = std::enable_if_t<F>>
  DriverType* driver() {
    static_assert(F == DriverOnForeground);

    auto* fg_driver = std::get_if<fdf_testing::DriverUnderTest<DriverType>>(&dut_);
    ZX_ASSERT_MSG(fg_driver, "Could not get the foreground driver variant.");
    return **fg_driver;
  }

 private:
  fdf_testing::DriverRuntime* runtime_;
  fdf::UnownedDispatcher dut_dispatcher_;
  std::variant<fdf_testing::DriverUnderTest<DriverType>,
               async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<DriverType>>>
      dut_;
};

// Helper macros to validate the incoming configuration.

#define SETUP_HAS_USING(name)                                                   \
  template <typename T, typename = void>                                        \
  struct Has##name : public ::std::false_type {};                               \
                                                                                \
  template <typename T>                                                         \
  struct Has##name<T, std::void_t<typename T::name>> : public std::true_type{}; \
                                                                                \
  template <typename T>                                                         \
  constexpr inline auto Has##name##V = Has##name<T>::value;

#define SETUP_HAS_CONSTEXPR(name)                                                   \
  template <typename T, typename = void>                                            \
  struct Has##name : public ::std::false_type {};                                   \
                                                                                    \
  template <typename T>                                                             \
  struct Has##name<T, std::void_t<decltype(T::k##name)>> : public std::true_type{}; \
                                                                                    \
  template <typename T>                                                             \
  constexpr inline auto Has##name##V = Has##name<T>::value;

SETUP_HAS_USING(DriverType)
SETUP_HAS_USING(EnvironmentType)
SETUP_HAS_CONSTEXPR(DriverOnForeground)
SETUP_HAS_CONSTEXPR(AutoStartDriver)
SETUP_HAS_CONSTEXPR(AutoStopDriver)

#undef SETUP_HAS_CONSTEXPR
#undef SETUP_HAS_USING

template <class Configuration>
class ConfigurationExtractor {
 public:
  // Validate DriverType.
  static_assert(HasDriverTypeV<Configuration>,
                "Ensure the Configuration class has defined a DriverType "
                "through a using statement: 'using DriverType = MyDriverType;'");
  using DriverType = typename Configuration::DriverType;

  // Validate EnvironmentType.
  static_assert(HasEnvironmentTypeV<Configuration>,
                "Ensure the Configuration class has defined an EnvironmentType "
                "through a using statement: 'using EnvironmentType = MyTestEnvironment;'");
  using EnvironmentType = typename Configuration::EnvironmentType;

  static_assert(std::is_base_of_v<fdf_testing::Environment, EnvironmentType>,
                "The EnvironmentType must implement the fdf_testing::Environment class.");
  static_assert(!std::is_abstract_v<EnvironmentType>, "The EnvironmentType cannot be abstract.");
  static_assert(std::is_constructible_v<EnvironmentType>,
                "The EnvironmentType must have a default constructor.");

  static_assert(
      HasDriverOnForegroundV<Configuration>,
      "Ensure the Configuration class has defined a 'static constexpr bool kDriverOnForeground'.");
  static constexpr bool kDriverOnForeground = Configuration::kDriverOnForeground;

  static_assert(
      HasAutoStartDriverV<Configuration>,
      "Ensure the Configuration class has defined a 'static constexpr bool kAutoStartDriver'.");
  static constexpr bool kAutoStartDriver = Configuration::kAutoStartDriver;

  static_assert(
      HasAutoStopDriverV<Configuration>,
      "Ensure the Configuration class has defined a 'static constexpr bool kAutoStopDriver'.");
  static constexpr bool kAutoStopDriver = Configuration::kAutoStopDriver;
};

}  // namespace internal

}  // namespace fdf_testing

#endif  // LIB_DRIVER_TESTING_CPP_FIXTURES_INTERNAL_FIXTURE_INTERNALS_H_
