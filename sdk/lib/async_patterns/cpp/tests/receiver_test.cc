// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/sync/cpp/completion.h>

#include <thread>

#include <gtest/gtest.h>
#include <sdk/lib/async_patterns/cpp/callback.h>
#include <sdk/lib/async_patterns/cpp/receiver.h>

#include "src/lib/testing/predicates/status.h"

namespace {

struct Owner {
 public:
  explicit Owner(async_dispatcher_t* dispatcher, int& count)
      : receiver_{this, dispatcher}, count_{count} {}

  void OnCallback(int arg) {
    EXPECT_EQ(arg, 42);
    count_++;
  }

  async_patterns::Callback<int> GetCallback() { return receiver_.Once(&Owner::OnCallback); }

  void OnCallbackMoveOnly(std::unique_ptr<int> arg) {
    EXPECT_EQ(*arg, 42);
    count_++;
  }

  async_patterns::Callback<std::unique_ptr<int>> GetCallbackMoveOnly() {
    return receiver_.Once(&Owner::OnCallbackMoveOnly);
  }

 private:
  async_patterns::Receiver<Owner> receiver_;
  int& count_;
};

TEST(Receiver, Receive) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  int count = 0;
  Owner owner{loop.dispatcher(), count};
  async_patterns::Callback<int> callback = owner.GetCallback();

  EXPECT_EQ(count, 0);
  callback(42);
  EXPECT_EQ(count, 0);
  ASSERT_OK(loop.RunUntilIdle());
  EXPECT_EQ(count, 1);
}

TEST(Receiver, ReceiveMoveOnly) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  int count = 0;
  Owner owner{loop.dispatcher(), count};
  async_patterns::Callback callback = owner.GetCallbackMoveOnly();

  EXPECT_EQ(count, 0);
  callback(std::make_unique<int>(42));
  EXPECT_EQ(count, 0);
  ASSERT_OK(loop.RunUntilIdle());
  EXPECT_EQ(count, 1);
}

TEST(Receiver, ReceiveConvertToFitCallback) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  int count = 0;
  Owner owner{loop.dispatcher(), count};
  async_patterns::Callback<int> callback = owner.GetCallback();
  // Since |callback| is just a functor, it should be adaptable to our standard
  // function types.
  fit::callback<void(int)> fit_callback{std::move(callback)};

  EXPECT_EQ(count, 0);
  fit_callback(42);
  EXPECT_EQ(count, 0);
  ASSERT_OK(loop.RunUntilIdle());
  EXPECT_EQ(count, 1);
}

TEST(Receiver, CannotReceiveAfterDestruction) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  int count = 0;
  std::optional<Owner> owner_store;
  owner_store.emplace(loop.dispatcher(), count);
  Owner& owner = *owner_store;
  async_patterns::Callback<int> callback = owner.GetCallback();

  EXPECT_EQ(count, 0);
  callback(42);
  EXPECT_EQ(count, 0);
  owner_store.reset();
  ASSERT_OK(loop.RunUntilIdle());
  EXPECT_EQ(count, 0);
}

TEST(Receiver, CannotReceiveAfterDispatcherShutdown) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  int count = 0;
  Owner owner{loop.dispatcher(), count};
  async_patterns::Callback<int> callback = owner.GetCallback();

  EXPECT_EQ(count, 0);
  callback(42);
  EXPECT_EQ(count, 0);
  loop.Shutdown();
  EXPECT_EQ(count, 0);
}

TEST(Receiver, CannotReceiveAfterDispatcherShutdownAndBothGoAway) {
  std::unique_ptr loop = std::make_unique<async::Loop>(&kAsyncLoopConfigNeverAttachToThread);

  int count = 0;
  std::unique_ptr owner = std::make_unique<Owner>(loop->dispatcher(), count);
  async_patterns::Callback<int> callback = owner->GetCallback();

  owner.reset();
  loop.reset();

  EXPECT_EQ(count, 0);
  callback(42);
  EXPECT_EQ(count, 0);
}

TEST(Receiver, CheckSynchronization) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  int count = 0;
  Owner owner{loop.dispatcher(), count};

  async_patterns::Callback<int> callback = owner.GetCallback();
  callback(42);

  // If |Owner| lives on the main thread, we cannot dispatch tasks to it
  // from an arbitrary thread.
  std::thread([&] { ASSERT_DEATH(loop.RunUntilIdle(), "thread-unsafe"); }).join();
}

TEST(Receiver, BindLambda) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  struct Owner {
   public:
    explicit Owner(async_dispatcher_t* dispatcher) : receiver_{this, dispatcher} {}

    async_patterns::Callback<int> GetCallback() {
      return receiver_.Once([](Owner* owner, int arg) {
        EXPECT_EQ(arg, 42);
        owner->count_++;
      });
    }

    // Won't compile due to static assertion.
#if 0
    async_patterns::Callback<int> GetCallbackWithStatefulLambda() {
      return receiver_.Once([this](Owner* owner, int arg) {
        EXPECT_EQ(arg, 42);
        owner->count_++;
      });
    }
#endif

    int count() const { return count_; }

   private:
    int count_ = 0;
    async_patterns::Receiver<Owner> receiver_;
  };
  Owner owner{loop.dispatcher()};
  EXPECT_EQ(owner.count(), 0);

  async_patterns::Callback<int> callback = owner.GetCallback();
  EXPECT_EQ(owner.count(), 0);

  callback(42);
  EXPECT_EQ(owner.count(), 0);

  ASSERT_OK(loop.RunUntilIdle());
  EXPECT_EQ(owner.count(), 1);
}

}  // namespace
