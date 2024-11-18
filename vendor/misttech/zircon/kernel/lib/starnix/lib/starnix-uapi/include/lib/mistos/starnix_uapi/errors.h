// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_ERRORS_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_ERRORS_H_

#include <lib/fit/result.h>
#include <lib/mistos/util/error.h>
#include <lib/zircon-internal/macros.h>
#include <zircon/types.h>

#include <source_location>
#include <utility>

#include <ktl/optional.h>
#include <ktl/string_view.h>

#include <linux/errno.h>

namespace starnix_uapi {

using mtl::BString;

class ErrnoCode {
 public:
  // impl ErrnoCode
  static ErrnoCode from_return_value(uint64_t retval) {
    int64_t rv = static_cast<int64_t>(retval);
    if (rv >= 0) {
      // Collapse all success codes to 0. This is the only value in the u32 range which
      // is guaranteed to not be an error code.
      return ErrnoCode(0);
    }
    return ErrnoCode(static_cast<uint32_t>(-rv));
  }

  static ErrnoCode from_error_code(int16_t code) { return ErrnoCode(static_cast<uint32_t>(code)); }

  uint64_t return_value() const { return -(static_cast<int64_t>(code_)); }

  uint32_t error_code() const { return code_; }

  BString to_string() const;

  // C++
  explicit ErrnoCode(uint32_t code, const char* name = "") : code_(code), name_(name) {}

  bool operator==(const ErrnoCode& other) const { return code_ == other.code_; }
  bool operator==(uint32_t other) const { return code_ == other; }

 private:
  uint32_t code_;
  const char* name_ = nullptr;
};

class Errno : public mtl::ext::StdError {
 public:
  ErrnoCode code_{0};

 private:
  std::source_location location_;
  ktl::optional<ktl::string_view> context_;

 public:
  // impl Errno
  static Errno New(const ErrnoCode& code,
                   std::source_location location = std::source_location::current()) {
    return Errno(code, location, ktl::nullopt);
  }

  static Errno with_context(const ErrnoCode& code, std::source_location location,
                            ktl::string_view context) {
    return Errno(code, location, context);
  }

  uint64_t return_value() const { return code_.return_value(); }

  BString to_string() const;

  // C++
  uint32_t error_code() const { return code_.error_code(); }

  bool operator==(const Errno& other) const { return code_ == other.code_; }
  bool operator==(const ErrnoCode& other) const { return code_ == other; }

 private:
  Errno(const ErrnoCode& code, std::source_location location,
        ktl::optional<ktl::string_view> context)
      : code_(code), location_(location), context_(context) {}
};

// Special errors indicating a blocking syscall was interrupted, but it can be restarted.
//
// They are not defined in uapi, but can be observed by ptrace on Linux.
//
// If the syscall is restartable, it might not be restarted, depending on the value of SA_RESTART
// for the signal handler and the specific restartable error code.
// But it will always be restarted if the signal did not call a userspace signal handler.
// If not restarted, this error code is converted into EINTR.
//
// More information can be found at
// https://cs.opensource.google/gvisor/gvisor/+/master:pkg/errors/linuxerr/internal.go;l=71;drc=2bb73c7bd7dcf0b36e774d8e82e464d04bc81f4b.

/// Convert to EINTR if interrupted by a signal handler without SA_RESTART enabled, otherwise
/// restart.
const ErrnoCode ERESTARTSYS(512, "ERESTARTSYS");

/// Always restart, regardless of the signal handler.
const ErrnoCode ERESTARTNOINTR(513, "ERESTARTNOINTR");

/// Convert to EINTR if interrupted by a signal handler. SA_RESTART is ignored. Otherwise restart.
const ErrnoCode ERESTARTNOHAND(514, "ERESTARTNOHAND");

/// Like `ERESTARTNOHAND`, but restart by invoking a closure instead of calling the syscall
/// implementation again.
const ErrnoCode ERESTART_RESTARTBLOCK(516, "ERESTART_RESTARTBLOCK");

fit::result<Errno> map_eintr(fit::result<Errno> result, const Errno& err);

template <typename T>
fit::result<Errno, T> map_eintr(fit::result<Errno, T> result, const Errno& err) {
  if (result.is_error()) {
    if (result.error_value().error_code() == EINTR) {
      return fit::error(err);
    }
    return result.take_error();
  }
  return result.take_value();
}

// There isn't really a mapping from zx_status::Status to Errno. The correct mapping is
// context-specific but this converter is a reasonable first-approximation. The translation matches
// fdio_status_to_errno. See https://fxbug.dev/42105838 for more context.
// TODO: Replace clients with more context-specific mappings.
uint32_t from_status_like_fdio(zx_status_t);

BString to_string(const std::source_location& location);

template <typename E, typename... T>
class SourceContext : public mtl::Context<E, T...> {
 public:
  using Base = mtl::Context<E, T...>;
  using Base::Base;  // Inherit constructors

  explicit SourceContext(fit::result<E, T...> result) : mtl::Context<E, T...>(std::move(result)) {}

  template <typename F>
  mtl::result<T...> with_source_context(
      F context_fn, std::source_location caller = std::source_location::current()) {
    return this->template with_context<BString>([&caller, &context_fn]() {
      const BString context = context_fn();
      auto caller_str = to_string(caller);
      return mtl::format("%.*s, %.*s", static_cast<int>(context.size()), context.data(),
                         static_cast<int>(caller_str.size()), caller_str.data());
    });
  }

  template <typename C>
  mtl::result<T...> source_context(C context,
                                   std::source_location caller = std::source_location::current()) {
    ktl::string_view context_sv = context;
    auto caller_str = to_string(caller);
    return this->context(mtl::format("%.*s, %.*s", static_cast<int>(context_sv.size()),
                                     context_sv.data(), static_cast<int>(caller_str.size()),
                                     caller_str.data()));
  }
};

// Helper function to create SourceContext wrapper
template <typename E, typename... T>
SourceContext<E, T...> make_source_context(fit::result<E, T...> result) {
  return SourceContext<E, T...>(std::move(result));
}

}  // namespace starnix_uapi

/// `errno` returns an `Errno` struct tagged with the current file name and line number.
///
/// Use `error!` instead if you want the `Errno` to be wrapped in an `Err`.

#define ERRNO_GET_MACRO(_1, _2, NAME, ...) NAME

#define ERRNO_1(err, name) \
  starnix_uapi::Errno::New(starnix_uapi::ErrnoCode(err, name), std::source_location::current())

#define ERRNO_2(err, name, ctx)                                         \
  starnix_uapi::Errno::with_context(starnix_uapi::ErrnoCode(err, name), \
                                    std::source_location::current(), ctx)

#define errno(err, ...) \
  ERRNO_GET_MACRO(err, ##__VA_ARGS__, ERRNO_2, ERRNO_1)(err, #err, ##__VA_ARGS__)

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_ERRORS_H_
