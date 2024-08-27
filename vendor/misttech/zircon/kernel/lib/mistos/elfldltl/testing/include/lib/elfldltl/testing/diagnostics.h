// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ELFLDLTL_TESTING_INCLUDE_LIB_ELFLDLTL_TESTING_DIAGNOSTICS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ELFLDLTL_TESTING_INCLUDE_LIB_ELFLDLTL_TESTING_DIAGNOSTICS_H_

#include <lib/elfldltl/diagnostics.h>
#include <lib/stdcompat/source_location.h>

#include <string_view>
#include <tuple>
#include <type_traits>
#include <utility>

#include <fbl/string.h>
#include <zxtest/zxtest.h>

namespace elfldltl::testing {

// Diagnostic flags for signaling as much information as possible.
struct TestingDiagnosticsFlags {
  template <auto>
  struct UniqueTrueType : public std::true_type {};

  [[no_unique_address]] UniqueTrueType<0> multiple_errors;
  [[no_unique_address]] UniqueTrueType<1> warnings_are_errors;
  [[no_unique_address]] UniqueTrueType<2> extra_checking;
};

struct ReportToString {
  template <typename... Args>
  inline fbl::String operator()(Args&&... args) const {
    // TODO (Herrera) Figure out how to implement
    // std::stringstream os;
    // auto report = OstreamDiagnosticsReport(os);
    // report(std::forward<decltype(args)>(args)...);
    // return std::move(os).str();
    return fbl::String();
  }
};

// This is a Report callable object that causes a gtest failure if called.
class ExpectNoReport {
 public:
  explicit constexpr ExpectNoReport(
      cpp20::source_location location = cpp20::source_location::current())
      : location_{location} {}

  template <typename... Args>
  constexpr bool operator()(Args&&... args) const {
    // TODO (Herrera) Figure out how to implement
    /*ADD_FAILURE() << location_.file_name() << ":" << location_.line() << ":" << location_.column()
                  << ": in " << location_.function_name() << ": Expected no diagnostics, not: "
                  << ReportToString{}(std::forward<Args>(args)...);*/
    return true;
  }

 private:
  cpp20::source_location location_;
};

// This is a Report callable object that checks for an expected sequence of
// arguments: calling with different arguments causes a gtest failure.
template <typename... Args>
class ExpectReport {
 public:
  static_assert(sizeof...(Args) > 0,
                "Expecting an empty .FormatError() call is invalid."
                " Use elfldltl::testing::ExpectOkDiagnostics instead.");

  static constexpr size_t kExpectedArgumentCount = sizeof...(Args);

  ExpectReport() = delete;

  constexpr ExpectReport(const ExpectReport& other) = delete;

  constexpr ExpectReport(ExpectReport&& other) noexcept
      : expected__{std::move(other.expected__)},
        state_{std::exchange(other.state_, State::kMoved)} {
    EXPECT_NE(state_, State::kMoved);
  }

  explicit constexpr ExpectReport(Args... args) : expected__{std::forward<Args>(args)...} {}

  template <typename... Ts>
  constexpr bool operator()(Ts&&... args) {
    constexpr size_t called_argument_count = sizeof...(Ts);
    switch (state_) {
      case State::kMoved:
      case State::kCalled:
        // This produces different failure messages when in these states.
        Diagnose(std::forward<Ts>(args)...);
        break;
      case State::kUncalled:
        if constexpr (called_argument_count != kExpectedArgumentCount) {
          EXPECT_EQ(called_argument_count, kExpectedArgumentCount,
                    "wrong number of arguments in Diagnostics error call");
          Diagnose(std::forward<Ts>(args)...);
        } else if (!Check(kSeq, args...)) {
          Diagnose(std::forward<Ts>(args)...);
        }
        break;
    }
    state_ = State::kCalled;  // Changes Diagnose behavior for next time.
    return true;              // Always ask to keep reporting more errors.
  }

  void ExpectCalledOrMoved() const {
    if (state_ == State::kUncalled) {
      // ADD_FAILURE() << "Missing expected error: " << std::apply(ReportToString{}, expected_);
    }
  }

  template <typename Report>
  bool ReportTo(Report&& report) {
    return std::apply(std::forward<Report>(report), expected__);
  }

 private:
  enum class State { kUncalled, kCalled, kMoved };

  template <typename T, typename T2 = void>
  struct NormalizeImpl {
    using type = T;
  };

  template <typename T>
  struct NormalizeImpl<T, std::enable_if_t<std::is_constructible_v<std::string_view, T>>> {
    using type = std::string_view;
  };

  template <typename T>
  struct NormalizeImpl<T, std::enable_if_t<std::is_integral_v<T>>> {
    using type = uint64_t;
  };

  template <typename T>
  using Normalize = typename NormalizeImpl<std::decay_t<T>>::type;

  using ExpectedArgs = std::tuple<Normalize<Args>...>;

  template <size_t I>
  using CheckType = std::tuple_element_t<I, ExpectedArgs>;

  static constexpr auto kSeq = std::make_index_sequence<sizeof...(Args)>();

  template <size_t... I, typename... Ts>
  [[nodiscard]] bool Check(std::index_sequence<I...> seq, Ts... args) const {
    return (CheckOne<I>(std::get<I>(expected__), std::forward<Ts>(args))
            // Use & instead of && to avoid short-circuiting in the fold:
            // Check and diagnose all arguments, not just the first mismatch.
            & ...);
  }

  template <size_t I, typename T>
  [[nodiscard]] static int CheckOne(CheckType<I> expected, T arg) {
    using CT = CheckType<I>;
    if constexpr (std::is_convertible_v<T, CT>) {
      CT normalized_arg = static_cast<CT>(std::move(arg));
      EXPECT_EQ(normalized_arg, expected, "argument %lu of %zu", I, kExpectedArgumentCount);
      return normalized_arg == expected;
    }
    /*ADD_FAILURE() << "incompatible types for argument " << I << " of " << kExpectedArgumentCount
                  << " " << __PRETTY_FUNCTION__;*/
    return false;
  }

  template <typename... Ts>
  void Diagnose(Ts&&... args) const {
    fbl::String formatted_arguments = ReportToString{}(std::forward<Ts>(args)...);
    fbl::String formatted_expected_arguments = std::apply(ReportToString{}, expected__);
    switch (state_) {
      case State::kMoved:
        /*ADD_FAILURE() << "Diagnostics used after std::move'd from!"
                      << "\nFor expected error: " << formatted_expected_arguments
                      << "\nCalled with: " << formatted_arguments;*/
        break;
      case State::kCalled:
        /*ADD_FAILURE() << "Expected only one error: " << formatted_expected_arguments
                      << "\nBut also got: " << formatted_arguments;*/
        break;
      case State::kUncalled:
        EXPECT_NE(formatted_arguments, formatted_expected_arguments,
                  "Diagnose with identical strings??");
        EXPECT_EQ(formatted_arguments, formatted_expected_arguments,
                  "Expected different Diagnostics arguments");
        break;
    }
  }

  ExpectedArgs expected__;
  State state_ = State::kUncalled;
};

// Deduction guide.
template <typename... Args>
ExpectReport(Args&&...) -> ExpectReport<Args...>;

// This Diagnostics object is constructed with expected error arguments.  If
// it's called with different arguments or not called exactly once before it
// goes out of scope, it causes a gtest failure.
template <typename... Args>
class ExpectedSingleError : public Diagnostics<ExpectReport<Args...>, TestingDiagnosticsFlags> {
 public:
  using ExpectedReport = ExpectReport<Args...>;
  using ExpectedDiagnostics = Diagnostics<ExpectedReport, TestingDiagnosticsFlags>;

  ExpectedSingleError(const ExpectedSingleError&) = delete;

  constexpr ExpectedSingleError(ExpectedSingleError&&) = default;

  explicit constexpr ExpectedSingleError(Args... args)
      : ExpectedDiagnostics{ExpectedReport{std::forward<Args>(args)...}} {}

  ~ExpectedSingleError() { this->report().ExpectCalledOrMoved(); }
};

// Deduction guide.
template <typename... Args>
ExpectedSingleError(Args&&...) -> ExpectedSingleError<Args...>;

// This Diagnostics object causes a gtest failure every time it's called.
struct ExpectOkDiagnostics : public Diagnostics<ExpectNoReport, TestingDiagnosticsFlags> {
  explicit constexpr ExpectOkDiagnostics(
      cpp20::source_location location = cpp20::source_location::current())
      : Diagnostics<ExpectNoReport, TestingDiagnosticsFlags>{ExpectNoReport{location}} {}

  constexpr ExpectOkDiagnostics(const ExpectOkDiagnostics&) = default;
};

// This collects a list of ExpectReport objects and applies consecutive calls
// to each in turn.  If it doesn't get a matching sequence of calls in order
// before it goes out of scope, it causes gtest failures.
template <class... Reports>
class ExpectReportList {
 public:
  static_assert(sizeof...(Reports) > 0,
                "elfldltl::testing::ExpectedErrorList invalid with no errors."
                " Use elfldltl::testing::ExpectOkDiagnostics instead.");

  ExpectReportList() = delete;

  ExpectReportList(const ExpectReportList&) = delete;

  constexpr ExpectReportList(ExpectReportList&& other)
      : reports_{std::move(other.reports_)}, next_{std::exchange(other.next_, kCount)} {}

  ExpectReportList& operator=(const ExpectReportList&) = delete;

  explicit constexpr ExpectReportList(Reports... reports) : reports_{std::move(reports)...} {}

  template <typename... Args>
  constexpr bool operator()(Args&&... args) {
    EXPECT_LT(next_, kCount, "too many errors");
    return next_ < kCount  //
               ? ExpectError(kSeq, next_++, std::forward<Args>(args)...)
               : ExpectNoReport{}(std::forward<Args>(args)...);
  }

  ~ExpectReportList() {
    EXPECT_EQ(next_, kCount, "wrong number of errors");
    auto expect_called = [](auto&... report) { (report.ExpectCalledOrMoved(), ...); };
    std::apply(expect_called, reports_);
  }

 private:
  static constexpr size_t kCount = sizeof...(Reports);
  static constexpr auto kSeq = std::make_index_sequence<kCount>();

  template <size_t... I, class... Args>
  constexpr bool ExpectError(std::index_sequence<I...> seq, size_t idx, Args&&... args) {
    return ((idx == I && std::get<I>(reports_)(std::forward<Args>(args)...)) || ...);
  }

  std::tuple<Reports...> reports_;
  size_t next_ = 0;
};

// Deduction guide.
template <class... Reports>
ExpectReportList(Reports&&...) -> ExpectReportList<std::decay_t<Reports>...>;

template <class... Reports>
struct ExpectedErrorList
    : public Diagnostics<ExpectReportList<Reports...>, TestingDiagnosticsFlags> {
  using ReportList = ExpectReportList<Reports...>;

  explicit constexpr ExpectedErrorList(Reports... reports)
      : Diagnostics<ReportList, TestingDiagnosticsFlags>{ReportList{std::move(reports)...}} {}
};

// Deduction guide.
template <class... Reports>
ExpectedErrorList(Reports&&...) -> ExpectedErrorList<std::decay_t<Reports>...>;

}  // namespace elfldltl::testing

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ELFLDLTL_TESTING_INCLUDE_LIB_ELFLDLTL_TESTING_DIAGNOSTICS_H_
