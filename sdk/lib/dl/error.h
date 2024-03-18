// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DL_ERROR_H_
#define LIB_DL_ERROR_H_

#include <cassert>
#include <cstdlib>
#include <string_view>
#include <utility>

namespace dl {

// The dl::Error object is created to hold an error string.  It's not created
// at all if there's no error.
//
// It's movable, but not copyable.  For convenience it can be constructed with
// printf-like arguments directly.  Most often it's only move-constructed or
// default-constructed.  It's move-assignable only when in its initial state or
// its moved-from state.
//
// Once created, then the object must be "set" and then must be "taken" before
// it's destroyed to avoid assertion failures.
//
// It's set by a call to the Printf method, or by constructing with printf-like
// arguments instead of default-constructing.  It's an assertion failure to
// call Printf on an object not in its default-constructed state.  In that
// state, it's an assertion failure to destroy the object without calling
// Printf on it.  It's an assertion failure to do a Printf call or construction
// that produces no output (like Printf("%s", "")).  Printf cannot fail, but
// also will not crash if memory allocation fails; instead it will act as if
// Printf("out of memory") had been called.
//
// The object is "taken" by calling take_str() or take_c_str().  Both return a
// pointer that is valid only for the lifetime of this dl::Error object.  After
// this, the object can only be destroyed or moved-from.  It must be kept alive
// as long as the string pointer is being used.
class Error {
 public:
  Error() = default;

  Error(const Error&) = delete;

  Error(Error&& other) noexcept { *this = std::move(other); }

  // This is like default-constructing and calling Printf.
  explicit Error(const char* format, ...);
  explicit Error(const char* format, va_list args) { Printf(format, args); }

  Error& operator=(const Error&) = delete;

  Error& operator=(Error&& other) {
    assert(!buffer_);
    buffer_ = std::exchange(other.buffer_, nullptr);
    size_ = std::exchange(other.size_, kMovedFrom);
    return *this;
  }

  // This must be called exactly once after default construction and before
  // anything else (except moving from or into the object).
  void Printf(const char* format, ...);
  void Printf(const char* format, va_list args);

  // This must be called exactly once after Printf has been called (or after
  // any non-default construction).  The returned string is valid only for the
  // lifetime of this Error object.  After this, the object can only be
  // destroyed, move-assigned (which also invalidates the string returned
  // here), or moved-from (which does not).
  std::string_view take_str() {
    assert(size_ != kUnused);
    assert(size_ != kTaken);
    assert(size_ != kMovedFrom);
    std::string_view str = "out of memory";
    if (buffer_) [[likely]] {
      assert(size_ < kSpecialSize);
      str = {buffer_, size_};
    } else {
      assert(size_ == kAllocationFailure);
    }
    size_ = kTaken;
    return str;
  }

  // This is the same as take_str(), but with a NUL-terminated C string.  One
  // X-or the other of take_str() and take_c_str() must be called exactly once.
  const char* take_c_str() { return take_str().data(); }

  ~Error() {
    // Must be taken or moved-from.  If moved-from, buffer_ must be nullptr.
    if (size_ != kTaken) {
      // Redundant assertions make each failure give more specific information.
      if (size_ < kSpecialSize) {
        assert(size_ != kUnused);
        assert(buffer_);
      } else {
        assert(!buffer_);
        assert(size_ != kAllocationFailure);
        assert(size_ == kMovedFrom);
      }
    }
    if (buffer_) {
      free(buffer_);
    }
  }

 private:
  // One of these values must be in size_ when buffer_ is nullptr.  When
  // buffer_ is set, size_ may be kTaken instead of the string's length.
  enum SpecialSize : size_t {
    kUnused = 0,

    // All size_ values >= kSpecialSize are also reserved.
    kSpecialSize = static_cast<size_t>(-3),
    kMovedFrom = kSpecialSize,
    kTaken,
    kAllocationFailure,
  };

  char* buffer_ = nullptr;
  size_t size_ = kUnused;
};

// This makes ostream << things like gtest macros take dl::Error destructively.
template <typename Ostream, typename T,
          typename = std::enable_if_t<std::is_same_v<Error, std::decay_t<T>>>>
constexpr decltype(auto) operator<<(Ostream&& os, Error&& error) {
  return std::forward<Ostream>(os) << error.take_str();
}

}  // namespace dl

#endif  // LIB_DL_ERROR_H_
