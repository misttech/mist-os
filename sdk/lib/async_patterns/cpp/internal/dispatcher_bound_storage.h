// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ASYNC_PATTERNS_CPP_INTERNAL_DISPATCHER_BOUND_STORAGE_H_
#define LIB_ASYNC_PATTERNS_CPP_INTERNAL_DISPATCHER_BOUND_STORAGE_H_

#include <lib/async/cpp/sequence_checker.h>
#include <lib/async/dispatcher.h>
#include <lib/async_patterns/cpp/callback.h>
#include <lib/async_patterns/cpp/sendable.h>
#include <lib/fit/function.h>
#include <lib/stdcompat/functional.h>
#include <lib/stdcompat/type_traits.h>
#include <zircon/assert.h>

#include <cstdlib>

namespace async_patterns::internal {

struct PassDispatcherT {
  // Pretend this is a dispatcher pointer to support |std::is_invocable| tests.
  // It is never called in practice.
  // NOLINTNEXTLINE(google-explicit-constructor)
  operator async_dispatcher_t*() const {
    __builtin_abort();
    return nullptr;
  }
};

constexpr char kDispatcherBoundThreadSafetyDescription[] =
    "|async_patterns::DispatcherBound| is meant to manage thread-unsafe asynchronous objects. "
    "Those objects should only be used from synchronized dispatchers. "
    "However, the |async_dispatcher_t*| provided is not a synchronized dispatcher.";

// |Synchronized<T>| checks for synchronization before using |T|.
template <typename T>
class Synchronized {
 public:
  template <typename... Args>
  explicit Synchronized(async_dispatcher_t* dispatcher, Args&&... args)
      : checker_(dispatcher, kDispatcherBoundThreadSafetyDescription),
        inner_(std::forward<Args>(args)...) {}

  /// Returns a callable that will check for synchronization before calling some
  /// member function of |T|.
  ///
  /// More precisely:
  ///
  /// - Let |fn| be the result of `bind_front(member_fn, &inner_)`.
  ///
  /// - Returns a callable that takes some arguments. When called, that callable
  ///   will check for synchronization, then forward those arguments to |fn|.
  template <typename Callable>
  auto Bind(Callable member_fn) {
    return [member_fn = std::move(member_fn), this](auto&&... args) mutable {
      std::lock_guard lock(checker_);
      return cpp20::bind_front(std::forward<Callable>(member_fn), &inner_)(std::move(args)...);
    };
  }

 private:
  async::synchronization_checker checker_;
  T inner_ __TA_GUARDED(checker_);
};

// |DispatcherBoundStorage| encapsulates the subtle work of managing memory
// across concurrency domains so |DispatcherBound| could be a minimal wrapper.
class DispatcherBoundStorage final {
 public:
  DispatcherBoundStorage() = default;
  ~DispatcherBoundStorage();

  bool has_value() const { return static_cast<bool>(op_fn_); }

  // Asynchronously constructs |T| on the |dispatcher|.
  //
  // |T| will be constructed and later destructed inside tasks posted to the
  // |dispatcher|.
  template <typename T, typename... Args>
  void Construct(async_dispatcher_t* dispatcher, Args&&... args) {
    ZX_ASSERT(!op_fn_);

    // We first allocate the underlying memory before posting any tasks,
    // so that the tasks have an agreed-upon memory location to construct
    // and destruct |T|, even if |DispatcherBoundStorage| is destroyed.
    Synchronized<T>* ptr = std::allocator<Synchronized<T>>{}.allocate(1);

    // |op_fn_| let us compactly store both the destructor and the pointer to
    // the managed object in one inlined type erasing object (a function)
    // without heap allocation.
    op_fn_ = [ptr](Operation op) -> void* {
      switch (op) {
        case Operation::kDestruct:
          ptr->~Synchronized<T>();
          std::allocator<Synchronized<T>>{}.deallocate(ptr, 1);
          return nullptr;
        case Operation::kGetPointer:
          // During |Call|, |ptr_| will be cast back to |T|. This is guaranteed to
          // yield the same object pointer by the language. See
          // https://timsong-cpp.github.io/cppwp/n4861/expr.static.cast#13
          return static_cast<void*>(ptr);
      }
    };

    ConstructInternal(dispatcher,
                      BindForSending(
                          [dispatcher, ptr](auto&&... args) {
                            new (ptr) Synchronized<T>(dispatcher, std::move(args)...);
                          },
                          ForwardOrPassDispatcher(dispatcher, std::forward<Args>(args))...));
  }

  template <typename T, typename Callable, typename... Args>
  void AsyncCall(async_dispatcher_t* dispatcher, Callable&& callable, Args&&... args) {
    void* raw_ptr = op_fn_(Operation::kGetPointer);
    auto* ptr = static_cast<Synchronized<T>*>(raw_ptr);
    CallInternal(dispatcher,
                 BindForSending(ptr->Bind(std::forward<Callable>(callable)),
                                ForwardOrPassDispatcher(dispatcher, std::forward<Args>(args))...));
  }

  template <template <typename> typename Builder, typename T, typename Callable, typename... Args>
  auto AsyncCallWithReply(async_dispatcher_t* dispatcher, Callable&& callable, Args&&... args) {
    void* raw_ptr = op_fn_(Operation::kGetPointer);
    Synchronized<T>* ptr = static_cast<Synchronized<T>*>(raw_ptr);
    auto make_task = [&] {
      return BindForSending(ptr->Bind(std::forward<Callable>(callable)),
                            ForwardOrPassDispatcher(dispatcher, std::forward<Args>(args))...);
    };
    return Builder<decltype(make_task())>(this, dispatcher, make_task());
  }

  template <typename Arg>
  auto ForwardOrPassDispatcher(async_dispatcher_t* dispatcher, Arg&& arg) {
    if constexpr (std::is_same_v<cpp20::remove_cvref_t<Arg>, PassDispatcherT>) {
      return Smuggle(dispatcher);
    } else {
      return std::forward<Arg>(arg);
    }
  }

  // Asynchronously destructs the object that was constructed earlier in
  // |Construct|.
  void Destruct(async_dispatcher_t* dispatcher);

  enum class Operation : uint8_t {
    kDestruct,
    kGetPointer,
  };

  static void ConstructInternal(async_dispatcher_t* dispatcher, fit::callback<void()> task);
  void CallInternal(async_dispatcher_t* dispatcher, fit::callback<void()> member);

 private:
  // |op_fn_| type-erases the managed object so |DispatcherBoundStorage| avoids
  // template bloat. See |DispatcherBoundStorage::Construct|.
  //
  // If |op_fn_| is valid, then the storage holds an object.
  fit::inline_function<void*(Operation)> op_fn_;
};

}  // namespace async_patterns::internal

#endif  // LIB_ASYNC_PATTERNS_CPP_INTERNAL_DISPATCHER_BOUND_STORAGE_H_
