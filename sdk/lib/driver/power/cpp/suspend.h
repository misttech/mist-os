// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_POWER_CPP_SUSPEND_H_
#define LIB_DRIVER_POWER_CPP_SUSPEND_H_

#include <fidl/fuchsia.power.system/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/zx/result.h>

#include <optional>
#include <type_traits>

namespace fdf_power {

namespace internal {
zx::result<fidl::ServerEnd<fuchsia_power_system::SuspendBlocker>> RegisterSuspendHooks(
    fdf::Namespace& incoming);
}

// This class is a wrapper for a callback type that must be called into exactly once
// before destruction. It is a move only type.
class Completer {
 public:
  explicit Completer(fit::callback<void()> callback) : callback_(std::move(callback)) {}

  Completer(Completer&& other) noexcept : callback_(std::move(other.callback_)) {
    other.callback_ = std::nullopt;
  }

  Completer(const Completer&) = delete;
  Completer& operator=(const Completer&) = delete;

  ~Completer();

  // Calls the wrapped callback function.
  // This method should not be invoked more than once.
  void operator()();

 private:
  std::optional<fit::callback<void()>> callback_;
};

// This is the completer for the Suspend operation in |Suspendable|.
class SuspendCompleter final : public Completer {
 public:
  using Completer::Completer;
  using Completer::operator();
};

// This is the completer for the Resume operation in |Suspendable|.
class ResumeCompleter final : public Completer {
 public:
  using Completer::Completer;
  using Completer::operator();
};

template <typename Driver>
class Suspendable {
 public:
  // Interface to be implemented.
  virtual void Suspend(SuspendCompleter completer) = 0;
  virtual void Resume(ResumeCompleter completer) = 0;

  explicit Suspendable() : server_(this) {
    static_cast<Driver*>(this)->RegisterInitMethods(
        fit::bind_member(this, &Suspendable::RegisterSuspendHooks));
  }

  virtual ~Suspendable() = default;

 private:
  class Server : public fidl::Server<fuchsia_power_system::SuspendBlocker> {
   public:
    explicit Server(Suspendable<Driver>* parent) : parent_(parent) {}

   private:
    void BeforeSuspend(BeforeSuspendCompleter::Sync& completer) override {
      parent_->Suspend(
          SuspendCompleter([completer = completer.ToAsync()]() mutable { completer.Reply(); }));
    }

    void AfterResume(AfterResumeCompleter::Sync& completer) override {
      parent_->Resume(
          ResumeCompleter([completer = completer.ToAsync()]() mutable { completer.Reply(); }));
    }

    void handle_unknown_method(
        fidl::UnknownMethodMetadata<fuchsia_power_system::SuspendBlocker> metadata,
        fidl::UnknownMethodCompleter::Sync& completer) override {}

    Suspendable<Driver>* parent_;
  };

  zx::result<> RegisterSuspendHooks(async_dispatcher_t* dispatcher, fdf::Namespace& incoming) {
    zx::result server_end = internal::RegisterSuspendHooks(incoming);
    if (server_end.is_error()) {
      return server_end.take_error();
    }
    binding_.emplace(dispatcher, std::move(server_end.value()), &server_,
                     fidl::kIgnoreBindingClosure);
    return zx::ok();
  }

  Server server_;
  std::optional<fidl::ServerBinding<fuchsia_power_system::SuspendBlocker>> binding_;
};

}  // namespace fdf_power

#endif
