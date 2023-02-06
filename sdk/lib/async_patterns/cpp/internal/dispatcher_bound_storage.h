// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ASYNC_PATTERNS_CPP_INTERNAL_DISPATCHER_BOUND_STORAGE_H_
#define LIB_ASYNC_PATTERNS_CPP_INTERNAL_DISPATCHER_BOUND_STORAGE_H_

#include <lib/async/dispatcher.h>
#include <lib/fit/function.h>
#include <lib/stdcompat/functional.h>
#include <lib/stdcompat/type_traits.h>
#include <zircon/assert.h>

#include <cstdlib>

#include <sdk/lib/async_patterns/cpp/callback.h>
#include <sdk/lib/async_patterns/cpp/sendable.h>

namespace async_patterns::internal {

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
    T* ptr = std::allocator<T>{}.allocate(1);

    // |op_fn_| let us compactly store both the destructor and the pointer to
    // the managed object in one inlined type erasing object (a function)
    // without heap allocation.
    op_fn_ = [ptr](Operation op) -> void* {
      switch (op) {
        case Operation::kDestruct:
          ptr->~T();
          std::allocator<T>{}.deallocate(ptr, 1);
          return nullptr;
        case Operation::kGetPointer:
          // During |Call|, |ptr_| will be cast back to |T|. This is guaranteed to
          // yield the same object pointer by the language. See
          // https://timsong-cpp.github.io/cppwp/n4861/expr.static.cast#13
          return static_cast<void*>(ptr);
      }
    };

    ConstructInternal(dispatcher,
                      BindForSending([ptr](auto&&... args) { new (ptr) T(std::move(args)...); },
                                     std::forward<Args>(args)...));
  }

  template <typename T, typename Member, typename... Args>
  void AsyncCall(async_dispatcher_t* dispatcher, Member T::*member, Args&&... args) {
    void* raw_ptr = op_fn_(Operation::kGetPointer);
    T* ptr = static_cast<T*>(raw_ptr);
    CallInternal(dispatcher,
                 BindForSending(cpp20::bind_front(member, ptr), std::forward<Args>(args)...));
  }

  template <typename Task>
  class [[nodiscard]] AsyncCallBuilder {
   public:
    template <typename R>
    void Then(async_patterns::Callback<R> on_result) && {
      ZX_DEBUG_ASSERT(storage_);
      static_assert(std::is_invocable_v<decltype(on_result), std::invoke_result_t<Task>>,
                    "The |async_patterns::Callback<R>| must accept the return value "
                    "of the |Member| being called.");
      storage_->CallInternal(dispatcher_,
                             [task = std::move(task_), on_result = std::move(on_result)]() mutable {
                               on_result(task());
                             });
      storage_ = nullptr;
    }

    AsyncCallBuilder(DispatcherBoundStorage* storage, async_dispatcher_t* dispatcher, Task task)
        : storage_(storage), dispatcher_(dispatcher), task_(std::move(task)) {}

    ~AsyncCallBuilder() { ZX_DEBUG_ASSERT(!storage_); }

    AsyncCallBuilder(const AsyncCallBuilder&) = delete;
    AsyncCallBuilder& operator=(const AsyncCallBuilder&) = delete;

    AsyncCallBuilder(AsyncCallBuilder&&) = delete;
    AsyncCallBuilder& operator=(AsyncCallBuilder&&) = delete;

   private:
    DispatcherBoundStorage* storage_;
    async_dispatcher_t* dispatcher_;
    Task task_;
  };

  template <typename T, typename Member, typename... Args>
  auto AsyncCallWithReply(async_dispatcher_t* dispatcher, Member T::*member, Args&&... args) {
    void* raw_ptr = op_fn_(Operation::kGetPointer);
    T* ptr = static_cast<T*>(raw_ptr);
    auto make_task = [&] {
      return BindForSending(cpp20::bind_front(member, ptr), std::forward<Args>(args)...);
    };
    return AsyncCallBuilder<decltype(make_task())>(this, dispatcher, make_task());
  }

  // Asynchronously destructs the object that was constructed earlier in
  // |Construct|.
  void Destruct(async_dispatcher_t* dispatcher);

 private:
  enum class Operation {
    kDestruct,
    kGetPointer,
  };

  static void ConstructInternal(async_dispatcher_t* dispatcher, fit::callback<void()> task);
  void CallInternal(async_dispatcher_t* dispatcher, fit::callback<void()> member);

  // |op_fn_| type-erases the managed object so |DispatcherBoundStorage| avoids
  // template bloat. See |DispatcherBoundStorage::Construct|.
  //
  // If |op_fn_| is valid, then the storage holds an object.
  fit::inline_function<void*(Operation)> op_fn_;
};

}  // namespace async_patterns::internal

#endif  // LIB_ASYNC_PATTERNS_CPP_INTERNAL_DISPATCHER_BOUND_STORAGE_H_
