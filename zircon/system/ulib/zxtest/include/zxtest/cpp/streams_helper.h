// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_SYSTEM_ULIB_ZXTEST_INCLUDE_ZXTEST_CPP_STREAMS_HELPER_H_
#define ZIRCON_SYSTEM_ULIB_ZXTEST_INCLUDE_ZXTEST_CPP_STREAMS_HELPER_H_

#include <sstream>

#include <zxtest/base/assertion.h>
#include <zxtest/base/types.h>
#include <zxtest/cpp/internal.h>

DECLARE_HAS_MEMBER_FN_WITH_SIGNATURE(has_status_value, status_value, zx_status_t (C::*)() const);
DECLARE_HAS_MEMBER_FN_WITH_SIGNATURE(has_status, status, zx_status_t (C::*)() const);

#define LIB_ZXTEST_RETURN_TAG_true return zxtest::internal::Tag{}&

#define LIB_ZXTEST_RETURN_TAG_false

#define LIB_ZXTEST_RETURN_TAG(val) LIB_ZXTEST_RETURN_TAG_##val

namespace zxtest::internal {

template <typename T>
zx_status_t GetStatus(const T& status) {
  if constexpr (has_status_value_v<T>) {
    return status.status_value();
  } else if constexpr (has_status_v<T>) {
    return status.status();
  } else {
    return status;
  }
}

struct Tag {};

class StreamableBase {
 public:
  StreamableBase(const zxtest::SourceLocation location)
      : stream_(std::stringstream("")), location_(location) {
    LIB_ZXTEST_CHECK_RUNNING();
  }

  // Lower precedence operator that returns void, such that the following
  // expressions are valid in functions that return void:
  //
  //  return Tag{} & StreamableBase{};
  //  return Tag{} & StreamableBase{} << "Stream operators are higher precedence than &";
  //
  friend void operator&(Tag, const StreamableBase&) {}

  template <typename T>
  StreamableBase& operator<<(T t) {
    stream_ << t;
    return *this;
  }

 protected:
  std::stringstream stream_;
  const zxtest::SourceLocation location_;
};

class StreamableFail : public StreamableBase {
 public:
  StreamableFail(const zxtest::SourceLocation location, bool is_fatal,
                 const cpp20::span<zxtest::Message*> traces)
      : StreamableBase(location), is_fatal_(is_fatal), traces_(traces) {}

  ~StreamableFail() {
    zxtest::Runner::GetInstance()->NotifyAssertion(
        zxtest::Assertion(stream_.str(), location_, is_fatal_, traces_));
  }

 private:
  bool is_fatal_ = false;
  cpp20::span<zxtest::Message*> traces_;
};

class StreamableAssertion : public StreamableBase {
 public:
  StreamableAssertion(const fbl::String& actual_val, const fbl::String& expected_val,
                      const char* actual_symbol, const char* expected_symbol,
                      const zxtest::SourceLocation location, bool is_fatal,
                      const cpp20::span<zxtest::Message*> traces)
      : StreamableBase(location),
        actual_value_(actual_val),
        expected_value_(expected_val),
        actual_symbol_(actual_symbol),
        expected_symbol_(expected_symbol),
        is_fatal_(is_fatal),
        traces_(traces) {}

  ~StreamableAssertion() {
    zxtest::Runner::GetInstance()->NotifyAssertion(
        zxtest::Assertion(stream_.str(), expected_symbol_, expected_value_, actual_symbol_,
                          actual_value_, location_, is_fatal_, traces_));
  }

 private:
  const fbl::String actual_value_;
  const fbl::String expected_value_;
  const char* actual_symbol_;
  const char* expected_symbol_;
  bool is_fatal_ = false;
  cpp20::span<zxtest::Message*> traces_;
};

class StreamableSkip : public StreamableBase {
 public:
  StreamableSkip(const zxtest::SourceLocation location) : StreamableBase(location) {}

  ~StreamableSkip() {
    zxtest::Message message(stream_.str(), location_);
    zxtest::Runner::GetInstance()->SkipCurrent(message);
  }
};

// Evaluates a condition and returns true if it is satisfied. If it is not, will create an assertion
// and notify the global runner instance.
template <typename Actual, typename Expected, typename CompareOp, typename PrintActual,
          typename PrintExpected>
std::unique_ptr<StreamableAssertion> EvaluateConditionForStream(
    const Actual& actual, const Expected& expected, const char* actual_symbol,
    const char* expected_symbol, const zxtest::SourceLocation& location, bool is_fatal,
    const CompareOp& compare, const PrintActual& print_actual,
    const PrintExpected& print_expected) {
  if (compare(actual, expected)) {
    return nullptr;
  }

  // Report the assertion error.
  fbl::String actual_value = print_actual(actual);
  fbl::String expected_value = print_expected(expected);
  return std::make_unique<StreamableAssertion>(actual_value, expected_value, actual_symbol,
                                               expected_symbol, location, is_fatal,
                                               zxtest::Runner::GetInstance()->GetScopedTraces());
}

}  // namespace zxtest::internal

#endif  // ZIRCON_SYSTEM_ULIB_ZXTEST_INCLUDE_ZXTEST_CPP_STREAMS_HELPER_H_
