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

class StdError : public ToString {
 public:
  virtual ~StdError() = default;

  virtual ktl::optional<const StdError*> source() const { return ktl::nullopt; }

  BString to_string() const override { return BString{"StdError"}; }
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

template <typename E>
struct ErrorImpl : public StdError {
  E object_;

  explicit ErrorImpl(E err) : object_(ktl::move(err)) {}

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
      return error.inner_;
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

  template <typename C, typename E>
  static Error from_context(C context, E error) {
    return Error::construct(ContextError<C, E>{ktl::move(context), ktl::move(error)});
  }

  BString to_string() const override {
    if (inner_) {
      return inner_->to_string();
    }
    return BString{"Error"};
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

template <class E>
class StdError {
 public:
  explicit StdError(E error) : error_(ktl::move(error)) {}

  template <typename C>
  Error ext_context(C context) {
    if constexpr (std::is_same_v<E, Error>) {
      return ktl::move(error_).context(context);
    }
    return Error::from_context(context, ktl::move(error_));
  }

 private:
  E error_;
};

}  // namespace ext

template <typename E, typename... T>
class Context : public fit::result<E, T...> {
 public:
  explicit Context(fit::result<E, T...> result) : fit::result<E, T...>(ktl::move(result)) {}

  /// Wrap the error value with additional context.
  template <typename Context>
  fit::result<Error, T...> context(Context&& context) {
    if (this->is_error()) {
      ext::StdError<E> error(ktl::move(this->error_value()));
      return fit::error(error.ext_context(context));
    }
    if constexpr (sizeof...(T) > 0) {
      return fit::ok(ktl::move(this->value()));
    } else {
      return fit::ok();
    }
  }

  /// Wrap the error value with additional context that is evaluated lazily
  /// only once an error does occur.
  template <typename Context, typename ContextFn>
    requires std::is_convertible_v<std::invoke_result_t<ContextFn>, Context>
  fit::result<Error, T...> with_context(ContextFn&& context_fn) {
    if (this->is_error()) {
      ext::StdError<E> error(ktl::move(this->error_value()));
      return fit::error(error.ext_context(context_fn()));
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
