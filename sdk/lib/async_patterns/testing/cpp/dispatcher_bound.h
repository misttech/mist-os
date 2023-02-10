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

  // Schedules an asynchronous |member| call on |dispatcher| with |args| and
  // then block the caller until the asynchronous call is processed, returning
  // the result of the asynchronous call. Example:
  //
  //     struct Foo {
  //       int Add(int a, int b) { return a + b; }
  //     };
  //     async_patterns::TestDispatcherBound<Foo> object{foo_loop.dispatcher()};
  //     int result = object.SyncCall(&Object::Add, 1, 2);  // result == 3
  //
  // If |member| returns |void|, use |AsyncCall| to schedule a fire-and-forget
  // call instead.
  template <typename Member, typename... Args>
  auto SyncCall(Member T::*member, Args&&... args) {
    return BlockOn(DispatcherBound<T>::AsyncCall(member, std::forward<Args>(args)...));
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

}  // namespace async_patterns

#endif  // LIB_ASYNC_PATTERNS_TESTING_CPP_DISPATCHER_BOUND_H_
