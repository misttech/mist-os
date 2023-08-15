// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ELFLDLTL_TESTING_INCLUDE_LIB_ELFLDLTL_TESTING_DIAGNOSTICS_H_
#define SRC_LIB_ELFLDLTL_TESTING_INCLUDE_LIB_ELFLDLTL_TESTING_DIAGNOSTICS_H_

#include <lib/elfldltl/diagnostics-ostream.h>
#include <lib/elfldltl/diagnostics.h>

#include <functional>
#include <sstream>
#include <string>
#include <string_view>
#include <tuple>
#include <type_traits>
#include <utility>

#include <gtest/gtest.h>

namespace elfldltl::testing {

// This helper object is instantiated with the expected error string and its
// diag() method returns a Diagnostics object.  When the helper object goes out
// of scope, it will assert that the Diagnostics object got the expected one
// error logged.
template <typename... Args>
class ExpectedSingleError {
 private:
  template <typename T, typename T2 = void>
  struct ExpectedType {
    using type = T;
  };

  template <typename T>
  struct ExpectedType<T, std::enable_if_t<std::is_constructible_v<std::string_view, T>>> {
    using type = std::string_view;
  };

  template <typename T>
  struct ExpectedType<T, std::enable_if_t<std::is_integral_v<T>>> {
    using type = uint64_t;
  };

 public:
  template <typename T>
  using expected_t = typename ExpectedType<T>::type;

  explicit ExpectedSingleError(Args... args) : expected_(std::move(args)...) {}

  auto& diag() { return diag_; }

  template <typename... Ts>
  bool operator()(Ts&&... args) const {
    constexpr size_t expected_argument_count = sizeof...(Args);
    constexpr size_t called_argument_count = sizeof...(Ts);
    if constexpr (called_argument_count != expected_argument_count) {
      EXPECT_EQ(called_argument_count, expected_argument_count);
      Diagnose(std::forward<Ts>(args)...);
    } else if (!Check(std::make_tuple(args...), std::make_index_sequence<sizeof...(Args)>())) {
      Diagnose(std::forward<Ts>(args)...);
    }
    return true;
  }

 private:
  // Diagnostic flags for signaling as much information as possible.
  static constexpr elfldltl::DiagnosticsFlags kFlags = {
      .multiple_errors = true,
      .warnings_are_errors = false,
      .extra_checking = true,
  };

  template <typename Tuple, size_t... I>
  [[nodiscard]] bool Check(Tuple&& args, std::index_sequence<I...> seq) const {
    return ([&]() -> int {  // Fold uses & instead of && so no short-circuit.
      const auto& raw_arg = std::get<I>(args);
      const auto& raw_expected = std::get<I>(expected_);
      using T = expected_t<std::decay_t<decltype(raw_expected)>>;
      if constexpr (std::is_convertible_v<decltype(raw_arg), T>) {
        const T arg = static_cast<T>(raw_arg);
        const T expected = raw_expected;
        EXPECT_EQ(arg, expected) << "argument " << I;
        return arg == expected;
      } else {
        ADD_FAILURE() << "incompatible types for argument " << I;
        return false;
      }
    }() & ...);
  }

  template <typename... Ts>
  void Diagnose(Ts&&... args) const {
    constexpr auto format = [](auto&&... args) -> std::string {
      std::stringstream os;
      auto report = OstreamDiagnosticsReport(os);
      report(std::forward<decltype(args)>(args)...);
      return std::move(os).str();
    };
    std::string formatted_arguments = format(std::forward<Ts>(args)...);
    if constexpr (sizeof...(Args) == 0) {
      EXPECT_EQ(formatted_arguments, "");
    } else {
      std::string formatted_expected_arguments = std::apply(format, expected_);
      EXPECT_EQ(formatted_arguments, formatted_expected_arguments);
    }
  }

  std::tuple<Args...> expected_;
  elfldltl::Diagnostics<std::reference_wrapper<ExpectedSingleError>> diag_{*this, kFlags};
};

template <typename... Args>
ExpectedSingleError(Args...)
    -> ExpectedSingleError<ExpectedSingleError<>::expected_t<std::decay_t<Args>>...>;

constexpr auto ExpectOkDiagnostics() {
  auto fail = [](std::string_view error, auto&&... args) {
    std::stringstream os;
    elfldltl::OstreamDiagnostics(os).FormatError(error, args...);
    std::string message = os.str();
    if (message.back() == '\n')
      message.pop_back();
    ADD_FAILURE() << "Expected no diagnostics, got \"" << message << '"';
    return false;
    ;
  };
  return elfldltl::Diagnostics(fail, elfldltl::DiagnosticsFlags{.extra_checking = true});
}

}  // namespace elfldltl::testing

#endif  // SRC_LIB_ELFLDLTL_TESTING_INCLUDE_LIB_ELFLDLTL_TESTING_DIAGNOSTICS_H_
