// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_FILE_H_
#define SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_FILE_H_

#include <lib/fit/result.h>

#include <optional>
#include <span>
#include <string_view>
#include <type_traits>
#include <utility>

#include "diagnostics.h"

namespace elfldltl {

template <typename Handle>
inline auto DefaultMakeInvalidHandle() {
  return Handle();
}

// elfldltl::File<Handle, Offset, Read, MakeInvalidHandle> implements the File
// API (see memory.h) by holding a Handle object and calling Read as a function
// fit::result<...>(Handle&, Offset, std::span<byte>) that returns some error
// value that Diagnostics::SystemError can handle.  MakeInvalidHandle can be
// supplied if default-construction isn't the way.
template <class Diagnostics, typename Handle, typename Offset, auto Read,
          auto MakeInvalidHandle = &DefaultMakeInvalidHandle<Handle>>
class File {
 public:
  static_assert(std::is_invocable_v<decltype(Read), Handle&, Offset, std::span<std::byte>>);
  static_assert(std::is_convertible_v<decltype(MakeInvalidHandle()), Handle>);

  using offset_type = Offset;

  File(const File&) noexcept(std::is_nothrow_copy_constructible_v<Handle>) = default;

  File(File&&) noexcept(std::is_nothrow_move_constructible_v<Handle>) = default;
  explicit File(Diagnostics& diag) : diag_(diag) {}

  File(Handle handle, Diagnostics& diag) noexcept(std::is_nothrow_move_constructible_v<Handle>)
      : handle_(std::move(handle)), diag_(diag) {}

  File& operator=(const File&) noexcept(std::is_nothrow_copy_assignable_v<Handle>) = default;

  File& operator=(File&&) noexcept(std::is_nothrow_move_assignable_v<Handle>) = default;

  const Handle& get() const { return handle_; }

  Handle release() { return std::exchange(handle_, MakeInvalidHandle()); }

  template <typename T>
  std::optional<T> ReadFromFile(Offset offset) {
    std::optional<T> result{std::in_place};
    std::span<T> data{std::addressof(result.value()), 1};
    FinishReadFromFile(offset, data, result);
    return result;
  }

  template <typename T, typename Allocator>
  auto ReadArrayFromFile(off_t offset, Allocator&& allocator, size_t count) {
    auto result = allocator(count);
    if (result) {
      std::span<T> data = *result;
      FinishReadFromFile(offset, data, result);
    }
    return result;
  }

 protected:
  Handle& handle() { return handle_; }
  const Handle& handle() const { return handle_; }

 private:
  template <typename T, typename Result>
  void FinishReadFromFile(Offset offset, std::span<T> data, Result& result) {
    using namespace std::string_view_literals;
    auto read = Read(handle_, offset, std::as_writable_bytes(data));
    if (read.is_error()) {
      // FileOffset takes an unsigned type, but off_t is signed.
      // No value passed to Read should be negative.
      auto diagnose = [offset = static_cast<std::make_unsigned_t<Offset>>(offset),
                       bytes = data.size_bytes(), this](auto&& error) {
        diag_.SystemError("cannot read "sv, bytes, " bytes"sv, FileOffset{offset}, ": "sv,
                          std::move(error));
      };
      auto error = std::move(read).error_value();
      if (error == decltype(error){}) {
        // The default-initialized error type is used to mean EOF.
        diagnose("reached end of file"sv);
      } else {
        diagnose(std::move(error));
      }
      result = Result();
    }
  }

  Handle handle_ = MakeInvalidHandle();
  Diagnostics& diag_;
};

}  // namespace elfldltl

#endif  // SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_FILE_H_
