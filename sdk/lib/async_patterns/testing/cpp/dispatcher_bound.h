// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ASYNC_PATTERNS_TESTING_CPP_DISPATCHER_BOUND_H_
#define LIB_ASYNC_PATTERNS_TESTING_CPP_DISPATCHER_BOUND_H_

#include <lib/async-loop/cpp/loop.h>
#include <lib/async_patterns/cpp/dispatcher_bound.h>
#include <lib/async_patterns/cpp/receiver.h>

namespace async_patterns {

// |TestDispatcherBound| adds the ability to issue synchronous calls on top of
// |DispatcherBound|. Synchronous test code may use this class instead of
// manually running an event loop.
//
// See |async_patterns::DispatcherBound| for general information.
template <typename T>
class TestDispatcherBound final : public DispatcherBound<T> {
 public:
  using DispatcherBound<T>::DispatcherBound;

  // Schedules an asynchronous |callable| on |dispatcher| with |args| and then
  // block the caller until the asynchronous call is processed, returning the
  // result of the asynchronous call.
  //
  // |callable| should take |T*| as the first argument, followed by optionally
  // |Args| if any. A special case of |callable| is a pointer-to-member, e.g.
  // |&T::SomeFunction|. Example:
  //
  //     struct Foo {
  //       int Add(int a, int b) { return a + b; }
  //     };
  //     async_patterns::TestDispatcherBound<Foo> object{foo_loop.dispatcher()};
  //
  //     int result = object.SyncCall(&Object::Add, 1, 2);  // result == 3
  //
  // General callables are also supported. The provided callable can safely
  // capture state without data races, because the current thread will be
  // suspended while the dispatcher thread is accessing the captures. However,
  // if you capture a pointer or reference, be careful to not let them escape to
  // the wrapped object. When in doubt, only use the captured data inside the
  // lambda scope:
  //
  //     int result = object.SyncCall([&] (Foo* foo) {
  //         return foo->Add(1, 2);
  //     });  // result == 3
  //
  // If |callable| returns |void|, |SyncCall| will still block until the
  // |callable| finishes executing on the target |dispatcher|. To schedule a
  // fire-and-forget call, use |AsyncCall|.
  template <typename Callable, typename... Args>
  auto SyncCall(Callable&& callable, Args&&... args) {
    constexpr bool kIsInvocable = std::is_invocable_v<Callable, T*, Args...>;
    static_assert(kIsInvocable,
                  "|Callable| must be callable with |T*| and the provided |Args|. "
                  "Check that you specified each argument correctly to the |callable| function.");
    if constexpr (kIsInvocable) {
      using Result = std::invoke_result_t<Callable, T*, Args...>;
      if constexpr (std::is_void_v<Result>) {
        // Wrap a void-returning function into one that returns a unit type.
        [[maybe_unused]] std::monostate unit = BlockOn(DispatcherBound<T>::UnsafeAsyncCallImpl(
            [callable = std::forward<Callable>(callable)](T* ptr, auto&&... args) mutable {
              std::invoke(callable, ptr, std::move(args)...);
              return std::monostate{};
            },
            std::forward<Args>(args)...));
        return;
      } else {
        return BlockOn(DispatcherBound<T>::UnsafeAsyncCallImpl(std::forward<Callable>(callable),
                                                               std::forward<Args>(args)...));
      }
    }
  }

 private:
  template <typename Task>
  std::invoke_result_t<Task> BlockOn(
      async_patterns::internal::DispatcherBoundStorage::AsyncCallBuilder<Task>&& builder) {
    using R = std::invoke_result_t<Task>;
    struct Slot {
      async::Loop loop{&kAsyncLoopConfigNeverAttachToThread};
      std::optional<R> result;
      async_patterns::Receiver<Slot> receiver{this, loop.dispatcher()};
    } slot;
    std::move(builder).Then(slot.receiver.Once([](Slot* slot, R result) {
      slot->result.emplace(std::move(result));
      slot->loop.Quit();
    }));
    ZX_ASSERT(slot.loop.Run() == ZX_ERR_CANCELED);
    return std::move(slot.result.value());
  }
};

// Constructs a |TestDispatcherBound<T>| that holds an instance of |T| by sending
// the |args| to the constructor of |T| run from a |dispatcher| task.
//
// See |TestDispatcherBound| constructor for details.
template <typename T, typename... Args>
TestDispatcherBound<T> MakeTestDispatcherBound(async_dispatcher_t* dispatcher, Args&&... args) {
  return TestDispatcherBound<T>{dispatcher, std::in_place, std::forward<Args>(args)...};
}

}  // namespace async_patterns

#endif  // LIB_ASYNC_PATTERNS_TESTING_CPP_DISPATCHER_BOUND_H_
