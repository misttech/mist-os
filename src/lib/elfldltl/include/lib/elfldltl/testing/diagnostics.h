// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_TEST_DIAGNOSTICS_H_
#define SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_TEST_DIAGNOSTICS_H_

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
 public:
  explicit ExpectedSingleError(Args&&... args) : expected_(args...) {}

  auto& diag() { return diag_; }

  template <typename... Ts>
  bool operator()(Ts&&... args) {
    if constexpr (sizeof...(Args) != sizeof...(Ts)) {
      ADD_FAILURE() << "Expected " << sizeof...(Args) << " args, got " << sizeof...(Ts);
    } else {
      Check(std::make_tuple(args...), std::make_index_sequence<sizeof...(Args)>());
    }
    return true;
  }

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

  template <typename Tuple, size_t... I>
  void Check(Tuple&& args, std::index_sequence<I...> seq) {
    (
        [&]() {
          using T = typename ExpectedType<decltype(std::get<I>(args))>::type;
          EXPECT_EQ(std::get<I>(args), T(std::get<I>(expected_))) << "argument " << I;
        }(),
        ...);
  }

 public:
  template <typename T>
  using expected_t = typename ExpectedType<T>::type;

 private:
  // Diagnostic flags for signaling as much information as possible.
  static constexpr elfldltl::DiagnosticsFlags kFlags = {
      .multiple_errors = true,
      .warnings_are_errors = false,
      .extra_checking = true,
  };

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

#endif  // SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_TEST_DIAGNOSTICS_H_
