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
                      BindFrontMove([ptr](auto&&... args) { new (ptr) T(std::move(args)...); },
                                    std::forward<Args>(args)...));
  }

  template <typename T, typename Member, typename... Args>
  void AsyncCall(async_dispatcher_t* dispatcher, Member T::*member, Args&&... args) {
    void* raw_ptr = op_fn_(Operation::kGetPointer);
    T* ptr = static_cast<T*>(raw_ptr);
    CallInternal(dispatcher,
                 BindFrontMove(cpp20::bind_front(member, ptr), std::forward<Args>(args)...));
  }

  // Similar to |std::bind_front|, but will first forward the |args| into the
  // lambda capture state, and then move the captured args into |callable| when
  // invoked later.
  template <typename Callable, typename... Args>
  auto BindFrontMove(Callable callable, Args&&... args) {
    // We package the arguments into a tuple because capturing a template
    // parameter pack is a C++20 feature.
    return [callable = std::move(callable),
            args = std::make_tuple(std::forward<Args>(args)...)]() mutable {
      // The |std::move| inside prevents |callable| from taking |Arg&| arguments.
      //
      // ** NOTE TO USERS ** If you get a compiler error pointing to this
      // vicinity, check the class documentation comments of |DispatcherBound|.
      // Passing mutable references and raw pointers are not allowed.
      std::apply(
          [callable = std::move(callable)](auto&&... args) mutable {
            auto check_arg = [](auto&& arg) {
              using Arg = decltype(arg);
              static_assert(!std::is_pointer_v<cpp20::remove_cvref_t<Arg>>,
                            "Sending raw pointers is not allowed. "
                            "See class documentation comments of |DispatcherBound|.");
              return true;
            };
            (void)(check_arg(std::forward<Args>(args)) && ...);

            callable(std::move(args)...);
          },
          args);
    };
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
