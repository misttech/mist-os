// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ASYNC_PATTERNS_CPP_DISPATCHER_BOUND_H_
#define LIB_ASYNC_PATTERNS_CPP_DISPATCHER_BOUND_H_

#include <lib/async/dispatcher.h>
#include <lib/async_patterns/cpp/internal/dispatcher_bound_storage.h>
#include <lib/fit/function.h>
#include <lib/stdcompat/functional.h>
#include <zircon/assert.h>

#include <cstdlib>

namespace async_patterns {

// |DispatcherBound<T>| enables an owner object living on some arbitrary thread,
// to construct, call methods on, and destroy an object of type |T| that must be
// used from a particular [synchronized async dispatcher][synchronized-dispatcher].
//
// Thread-unsafe asynchronous types should be used from synchronized dispatchers
// (e.g. a single-threaded async loop). Because the dispatcher may be running
// code to manipulate such objects, one should not use the same objects from
// other unrelated threads and cause data races.
//
// However, it may not always be possible for an entire tree of objects to
// live on the same async dispatcher, due to design or legacy constraints.
// |DispatcherBound| helps one divide classes along dispatcher boundaries.
//
// An example:
//
//     // |Background| always lives on a background dispatcher, provided
//     // at construction time.
//     class Background {
//      public:
//       explicit Background(async_dispatcher_t* dispatcher) {
//         // Perform some asynchronous work. The work is canceled if
//         // |Background| is destroyed.
//         task_.Post(dispatcher);
//       }
//
//      private:
//       void DoSomething();
//
//       // |task_| manages an async task that borrows the containing
//       // |Background| object and is not thread safe. It must be destroyed
//       // on the dispatcher to ensure that task cancellation is not racy.
//       async::TaskClosureMethod<Background, &Background::DoSomething> task_{this};
//     };
//
//     class Owner {
//      public:
//       // Asynchronously constructs a |Background| object on its dispatcher.
//       // Code in |Owner| and code in |Background| may run concurrently.
//       explicit Owner() :
//           background_loop_(&kAsyncLoopConfigNoAttachToCurrentThread),
//           background_{background_loop_.dispatcher()} {}
//
//      private:
//       // The async loop which will manage |Background| objects.
//       // This will always be paired with a |DispatcherBound| object.
//       async::Loop background_loop_;
//
//       // The |DispatcherBound| which manages |Background| on its loop.
//       // During destruction, |background_| will schedule the asynchronous
//       // destruction of the wrapped |Background| object on the dispatcher.
//       async_patterns::DispatcherBound<Background> background_;
//     };
//
// |DispatcherBound| itself is thread-compatible.
//
// ## Safety of sending arguments
//
// When constructing |T| and calling member functions of |T|, it is possible to
// pass additional arguments if the constructor or member function requires it.
// The argument will be forwarded from the caller's thread into a heap data
// structure, and later moved into the thread which would run the dispatcher
// task asynchronously. Each argument must be safe to send to a different
// thread:
//
// - Value types may be sent via copying or moving. Whether one passes an |Arg|,
//   |const Arg&|, or |Arg&&| etc on the sending side, the receiving |T| will
//   have its own uniquely owned instance that they may safely manipulate.
//
// - Move-only types such as |std::unique_ptr<Arg>| may be sent via |std::move|.
//
// - The constructor or member function of |T| may not take raw pointer or
//   non-const reference arguments. Sending those may result in use-after-frees
//   when the dispatcher uses a pointee asynchronously after an unspecified
//   amount of time. It's not worth the memory safety risks to support the cases
//   where a pointee outlives the dispatcher that is running tasks for |T|.
//
// - If the constructor or member function takes |const Arg&| as an argument,
//   one may send an instance of |Arg| via copying or moving. The instance will
//   live until the destination constructor or member function finishes
//   executing.
//
// - When sending a ref-counted pointer such as |std::shared_ptr<Arg>|, both the
//   sender and the receiver will be able to concurrently access |Arg|. Be sure
//   to add appropriate synchronization to |Arg|, for example protecting fields
//   with mutexes. This pattern is called "shared-state concurrency".
//
// - These checks are performed for each argument, but there could still be raw
//   pointers or references inside an object that is sent. One must ensure that
//   the pointee remain alive until the asynchronous tasks are run, possibly
//   indefinitely until the dispatcher is destroyed.
//
// [synchronized-dispatcher]:
// https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/thread-safe-async#synchronized-dispatcher
template <typename T>
class DispatcherBound final {
 public:
  // Constructs a |DispatcherBound| that does not hold an instance of |T|.
  DispatcherBound() = default;

  // Asynchronously constructs |T| on a task posted to |dispatcher|.
  //
  // See class documentation comments for the requirements on |args|.
  //
  // Panics if |dispatcher| cannot fulfill the task (e.g. it is shutdown).
  template <typename... Args>
  explicit DispatcherBound(async_dispatcher_t* dispatcher, Args&&... args)
      : dispatcher_(dispatcher) {
    storage_.Construct<T>(dispatcher, std::forward<Args>(args)...);
  }

  // Asynchronously constructs |T| on a task posted to |dispatcher|.
  //
  // If this object already holds an instance of |T|, that older instance will
  // be asynchronously destroyed on the existing dispatcher it was associated with.
  //
  // See class documentation comments for the requirements on |args|.
  //
  // Panics if either dispatcher cannot fulfill the task (e.g. it is shutdown).
  template <typename... Args>
  void emplace(async_dispatcher_t* dispatcher, Args&&... args) {
    reset();
    dispatcher_ = dispatcher;
    storage_.Construct<T>(dispatcher, std::forward<Args>(args)...);
  }

  // Asynchronously calls |member|, a pointer to member function of |T|, using
  // the provided |args|.
  //
  // |member| must return void.
  //
  // See class documentation comments for the requirements on |args|.
  //
  // Panics if the dispatcher cannot fulfill the task (e.g. it is shutdown).
  template <typename Member, typename... Args>
  void AsyncCall(Member T::*member, Args&&... args) {
    static_assert(std::is_void_v<std::invoke_result_t<Member, Args...>>,
                  "|member| must be a pointer to member function that returns void.");
    storage_.AsyncCall<T>(dispatcher_, member, std::forward<Args>(args)...);
  }

  // Typically, asynchronous classes would contain internal self-pointers that
  // make moving dangerous, so we disable moves here for now.
  DispatcherBound(DispatcherBound&&) noexcept = delete;
  DispatcherBound& operator=(DispatcherBound&&) noexcept = delete;

  DispatcherBound(const DispatcherBound&) noexcept = delete;
  DispatcherBound& operator=(const DispatcherBound&) noexcept = delete;

  // If |has_value|, asynchronously destroys the managed |T| on a task
  // posted to the dispatcher.
  //
  // Panics if the dispatcher cannot fulfill the task (e.g. it is shutdown).
  ~DispatcherBound() { reset(); }

  // If |has_value|, asynchronously destroys the managed |T| on a task
  // posted to the dispatcher.
  //
  // Panics if the dispatcher cannot fulfill the task (e.g. it is shutdown).
  void reset() {
    if (!has_value()) {
      return;
    }
    storage_.Destruct(dispatcher_);
  }

  // Returns if this object holds an instance of |T|.
  bool has_value() const { return storage_.has_value(); }

 private:
  async_dispatcher_t* dispatcher_;
  internal::DispatcherBoundStorage storage_;
};

}  // namespace async_patterns

#endif  // LIB_ASYNC_PATTERNS_CPP_DISPATCHER_BOUND_H_
