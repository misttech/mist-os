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
// thread. See |async_patterns::BindForSending| for the detailed requirements.
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
  // See |async_patterns::BindForSending| for detailed requirements on |args|.
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
  // See |async_patterns::BindForSending| for detailed requirements on |args|.
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
  // If |member| returns void, then |AsyncCall| returns void. The behavior is
  // fire-and-forget.
  //
  // If |member| returns some |R| type, then |AsyncCall| returns a builder
  // object where one must attach an |async_patterns::Callback<R'>| by calling
  // |Then|. |R'| could be identical to |R| or some other compatible type such
  // as |const R&|. Typically, the owner will declare a |Receiver| to mint those
  // callbacks:
  //
  //     class Owner {
  //      public:
  //       Owner(async_dispatcher_t* owner_dispatcher) : receiver_{this, owner_dispatcher} {
  //         // Tell |background_| to |DoSomething|, then send back the return
  //         // value to |Owner| using |receiver_|.
  //         background_
  //             .AsyncCall(&Background::DoSomething)
  //             .Then(receiver_.Once(&Owner::DoneSomething));
  //       }
  //
  //       void DoneSomething(Result result) {
  //         // |Background::DoSomething| has completed with |result|...
  //       }
  //
  //      private:
  //       async::Loop background_loop_;
  //       async_patterns::DispatcherBound<Background> background_{background_loop_.dispatcher()};
  //       async_patterns::Receiver<Owner> receiver_;
  //     };
  //
  // See |async_patterns::BindForSending| for detailed requirements on |args|.
  //
  // Panics if the dispatcher cannot fulfill the task (e.g. it is shutdown).
  template <typename Member, typename... Args,
            typename = std::enable_if_t<std::is_void_v<std::invoke_result_t<Member, Args...>>>>
  void AsyncCall(Member T::*member, Args&&... args) {
    storage_.AsyncCall<T>(dispatcher_, member, std::forward<Args>(args)...);
  }
  template <typename Member, typename... Args,
            typename = std::enable_if_t<!std::is_void_v<std::invoke_result_t<Member, Args...>>>>
  auto AsyncCall(Member T::*member, Args&&... args) {
    return storage_.AsyncCallWithReply<T>(dispatcher_, member, std::forward<Args>(args)...);
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
