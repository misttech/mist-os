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

  virtual ktl::optional<const StdError*> source() const { return ktl::nullopt; }

  virtual BString to_string() const { return "StdError"; }
};

class Chain {
 public:
  explicit Chain(const StdError* head) : next_(head) {}

  class Iterator {
   public:
    explicit Iterator(const StdError* error) : current_(error) {}

    const StdError* operator*() const { return current_; }

    Iterator& operator++() {
      if (current_) {
        auto next = current_->source();
        current_ = next ? *next : nullptr;
      }
      return *this;
    }

    bool operator==(const Iterator& other) const { return current_ == other.current_; }
    bool operator!=(const Iterator& other) const { return !(*this == other); }

   private:
    const StdError* current_;
  };

  Iterator begin() const { return Iterator(next_); }
  Iterator end() const { return Iterator(nullptr); }

 private:
  const StdError* next_;
};

template <typename E = void>
struct ErrorImpl : public StdError {
  E object_;

  explicit ErrorImpl(E err) : object_(std::move(err)) {}

  BString to_string() const override { return object_.to_string(); }

  ktl::optional<const StdError*> source() const override {
    if constexpr (std::is_base_of_v<StdError, E>) {
      return object_.source();
    }
    return ktl::nullopt;
  }
};

class Error;

template <typename C, typename E>
struct ContextError : public StdError {
  C context;
  E error;

  ContextError(C ctx, E err) : context(ktl::move(ctx)), error(ktl::move(err)) {}

  BString to_string() const final {
    ktl::string_view sv = this->context;
    return format("%.*s", static_cast<int>(sv.size()), sv.data());
  }

  ktl::optional<const StdError*> source() const override {
    if constexpr (std::is_same_v<E, Error>) {
      return error.source();
    }
    if constexpr (std::is_base_of_v<StdError, E>) {
      return static_cast<const StdError*>(&error);
    }
    return ktl::nullopt;
  }
};

template <typename M>
class MessageError : public StdError {
 public:
  explicit MessageError(M message) : message_(ktl::move(message)) {}

  BString to_string() const override { return this->message_; }

 private:
  M message_;
};

class Error : public StdError {
 private:
  // Non-null ptr;
  StdError* inner_ = nullptr;

 public:
  template <typename E>
  explicit Error(E&& error) {
    fbl::AllocChecker ac;
    auto ptr = new (&ac) ErrorImpl<std::decay_t<E>>(std::forward<E>(error));
    ZX_ASSERT(ac.check());
    inner_ = ptr;
  }

  Error(Error&& other) : inner_(other.inner_) { other.inner_ = nullptr; }
  Error& operator=(Error&& other) noexcept {
    if (this != &other) {
      delete inner_;
      inner_ = other.inner_;
      other.inner_ = nullptr;
    }
    return *this;
  }

  Error(const Error&) = delete;
  Error& operator=(const Error&) = delete;

  ~Error() {
    delete inner_;
    inner_ = nullptr;
  }

  template <typename M>
  static Error msg(M message) {
    return Error::construct(MessageError<M>(message));
  }

  template <typename C>
  Error context(C context) && {
    return Error::construct(ContextError<C, Error>{ktl::move(context), ktl::move(*this)});
  }

  template <typename C>
  Error ext_context(C ctx) const {
    return this->context(ctx);
  }

  template <typename C, typename E>
  static Error from_context(C context, E error) {
    return Error::construct(ContextError<C, E>{ktl::move(context), ktl::move(error)});
  }

  BString to_string() const override {
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

      auto chain = Chain(inner_);
      auto it = chain.begin();
      if (it != chain.end()) {
        ++it;
      }

      for (; it != chain.end(); ++it) {
        auto err_str = (*it)->to_string();
        printer(":%.*s", static_cast<int>(err_str.size()), err_str.data());
      }
    }
  }

  ktl::optional<const StdError*> source() const override {
    if (inner_) {
      return inner_;
    }
    return ktl::nullopt;
  }

 private:
  template <typename C, typename E>
  friend struct ContextError;

  template <typename E>
  static Error construct(E error) {
    return Error(ktl::move(error));
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
      auto error = ktl::move(this->error_value());
      if constexpr (std::is_base_of_v<ext::StdError, std::remove_cvref_t<decltype(error)>>) {
        return fit::error(error.ext_context(context));
      }
      return fit::error(Error::from_context(context, ktl::move(error)));
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
      auto error = ktl::move(this->error_value());
      if constexpr (std::is_base_of_v<ext::StdError, std::remove_cvref_t<decltype(error)>>) {
        return fit::error(error.ext_context(context_fn()));
      }
      return fit::error(Error::from_context(context_fn(), ktl::move(error)));
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
