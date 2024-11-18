// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_ERROR_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_ERROR_H_

#include <lib/fit/result.h>
#include <lib/mistos/util/bstring.h>

#include <ktl/optional.h>
#include <ktl/string_view.h>

namespace mtl {

class StdError {
 public:
  virtual ~StdError() = default;

  virtual ktl::optional<StdError*> source() const { return ktl::nullopt; }

  virtual BString to_string() const { return "StdError"; }
};

template <typename E = void>
struct ErrorImpl : public StdError {
  E object_;

  explicit ErrorImpl(E err) : object_(std::move(err)) {}

  BString to_string() const override { return object_.to_string(); }
};

template <typename C, typename E>
struct ContextError {
  C context;
  E error;

  BString to_string() const {
    ktl::string_view sv = this->context;
    return format("%.*s", static_cast<int>(sv.size()), sv.data());
  }
};

template <typename M>
class MessageError {
 public:
  explicit MessageError(M message) : message_(ktl::move(message)) {}

  BString to_string() const { return this->message_; }

 private:
  M message_;
};

class Error {
 private:
  StdError* inner_;

 public:
  template <typename E>
  explicit Error(E&& error) {
    fbl::AllocChecker ac;
    auto ptr = new (&ac) ErrorImpl<std::decay_t<E>>(std::forward<E>(error));
    ZX_ASSERT(ac.check());
    inner_ = ktl::move(ptr);
  }

  template <typename M>
  static Error msg(M message) {
    return Error::construct(MessageError<M>(message));
  }

  template <typename C>
  Error context(C context) const {
    ContextError e{.context = context, .error = *this};
    return Error::construct(e);
  }

  template <typename C>
  Error ext_context(C ctx) const {
    return this->context(ctx);
  }

  template <typename C, typename E>
  static Error from_context(C context, E error) {
    ContextError e{.context = context, .error = error};
    return Error::construct(e);
  }

  BString to_string() const {
    if (inner_) {
      return inner_->to_string();
    }
    return "Error";
  }

  template <typename Printer>
  void print(Printer&& printer) const {
    if (inner_) {
      auto str = inner_->to_string();
      printer("%.*s", static_cast<int>(str.size()), str.data());
    }
  }

  /*~Error() {
    delete inner_;
    inner_ = nullptr;
  }*/

 private:
  template <typename E>
  static Error construct(const E& error) {
    return Error(error);
  }
};

namespace ext {

class StdError {
 public:
  template <typename C>
  Error ext_context(C context) const {
    return Error::from_context(context, *this);
  }
};

}  // namespace ext

template <typename E, typename... T>
class Context : public fit::result<E, T...> {
 public:
  using Base = fit::result<E, T...>;
  using Base::Base;  // Inherit constructors

  explicit Context(fit::result<E, T...> result) : fit::result<E, T...>(std::move(result)) {}

  /// Wrap the error value with additional context.
  template <typename C>
  fit::result<Error, T...> context(C context) {
    if (this->is_error()) {
      const auto& error = this->error_value();
      if constexpr (std::is_base_of_v<ext::StdError, std::remove_cvref_t<decltype(error)>>) {
        return fit::error(error.ext_context(context));
      }
      return fit::error(Error::from_context(context, error));
    }
    if constexpr (sizeof...(T) > 0) {
      return fit::ok(ktl::move(this->value()));
    } else {
      return fit::ok();
    }
  }

  /// Wrap the error value with additional context that is evaluated lazily
  /// only once an error does occur.
  template <typename C, typename F>
    requires std::is_convertible_v<std::invoke_result_t<F>, C>
  fit::result<Error, T...> with_context(F&& context_fn) {
    if (this->is_error()) {
      const auto& error = this->error_value();
      if constexpr (std::is_base_of_v<ext::StdError, std::remove_cvref_t<decltype(error)>>) {
        return fit::error(error.ext_context(context_fn()));
      }
      return fit::error(Error::from_context(context_fn(), error));
    }
    if constexpr (sizeof...(T) > 0) {
      return fit::ok(ktl::move(this->value()));
    } else {
      return fit::ok();
    }
  }
};

template <typename... T>
using result = fit::result<Error, T...>;

}  // namespace mtl

// Helper macro for error formatting
#define anyhow(x, ...) mtl::Error::msg(mtl::format(x, ##__VA_ARGS__))

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_ERROR_H_
