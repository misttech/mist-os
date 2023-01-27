// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async_patterns/cpp/dispatcher_bound.h>
#include <lib/sync/cpp/completion.h>

#include <thread>

#include <gtest/gtest.h>

#include "src/lib/testing/predicates/status.h"

namespace {

// In this test fixture, the loop should always be run from the main thread.
class DispatcherBound : public testing::Test {
 protected:
  void SetUp() override { loop_thread_id_ = std::this_thread::get_id(); }

  void TearDown() override {}

  async::Loop& loop() { return loop_; }

  std::thread::id loop_thread_id() const { return loop_thread_id_; }

 private:
  std::thread::id loop_thread_id_;
  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
};

using ArcAtomic = std::shared_ptr<std::atomic_int>;
ArcAtomic make_arc_atomic() { return std::make_shared<std::atomic_int>(); }

// |ThreadAffine| asserts that it is always used from a particular thread.
// It will be used to check that |DispatcherBound| schedules all work on the
// dispatcher thread.
class ThreadAffine {
 public:
  explicit ThreadAffine(std::thread::id expected, ArcAtomic counter)
      : expected_(expected), counter_(std::move(counter)) {
    EXPECT_EQ(expected_, std::this_thread::get_id());
    counter_->fetch_add(1);
  }

  ~ThreadAffine() {
    EXPECT_EQ(expected_, std::this_thread::get_id());
    counter_->fetch_sub(1);
  }

  // The following are a bunch of thread-affine operations that take different
  // kinds of data types and exercised in the tests later.

  void Add(int a, int b, const ArcAtomic& result) {
    EXPECT_EQ(expected_, std::this_thread::get_id());
    result->store(a + b);
  }

  void PassMoveOnly(std::unique_ptr<int> p) {
    EXPECT_EQ(expected_, std::this_thread::get_id());
    EXPECT_NE(nullptr, p.get());
  }

  void PassPointer(std::string* s) {
    EXPECT_EQ(expected_, std::this_thread::get_id());
    EXPECT_NE("", *s);
    *s = "";
  }

  void PassReference(std::string& s) {
    EXPECT_EQ(expected_, std::this_thread::get_id());
    EXPECT_NE("", s);
    s = "";
  }

  void PassConstReference(const std::string& s) {
    EXPECT_EQ(expected_, std::this_thread::get_id());
    EXPECT_NE("", s);
  }

  void PassValue(std::vector<int> v) {
    EXPECT_EQ(expected_, std::this_thread::get_id());
    EXPECT_GT(v.size(), 0u);
    v = {};
  }

 private:
  std::thread::id expected_;
  ArcAtomic counter_;
};

TEST_F(DispatcherBound, DefaultConstruction) {
  async_patterns::DispatcherBound<ThreadAffine> obj;
  EXPECT_FALSE(obj.has_value());
}

TEST_F(DispatcherBound, Construct) {
  libsync::Completion create;
  libsync::Completion destroy;
  auto object_count = make_arc_atomic();

  // From a foreign thread |t1|, we schedule the creation and destruction of a
  // |ThreadAffine| object onto the loop.
  std::thread t1{[&] {
    async_patterns::DispatcherBound<ThreadAffine> obj{loop().dispatcher(), loop_thread_id(),
                                                      object_count};
    EXPECT_TRUE(obj.has_value());
    create.Signal();

    destroy.Wait();
    obj.reset();
    EXPECT_FALSE(obj.has_value());
  }};

  EXPECT_OK(create.Wait());
  EXPECT_EQ(0, object_count->load());

  EXPECT_OK(loop().RunUntilIdle());
  EXPECT_EQ(1, object_count->load());

  destroy.Signal();
  t1.join();
  EXPECT_EQ(1, object_count->load());

  EXPECT_OK(loop().RunUntilIdle());
  EXPECT_EQ(0, object_count->load());
}

TEST_F(DispatcherBound, Emplace) {
  libsync::Completion did_create_1;
  libsync::Completion will_create_2;
  libsync::Completion did_create_2;
  libsync::Completion will_destroy;
  auto object_count_1 = make_arc_atomic();
  auto object_count_2 = make_arc_atomic();

  // From a foreign thread |t1|, we schedule the creation and destruction of
  // two |ThreadAffine| objects onto the loop.
  std::thread t1{[&] {
    async_patterns::DispatcherBound<ThreadAffine> obj;
    obj.emplace(loop().dispatcher(), loop_thread_id(), object_count_1);
    EXPECT_TRUE(obj.has_value());
    did_create_1.Signal();

    will_create_2.Wait();
    obj.emplace(loop().dispatcher(), loop_thread_id(), object_count_2);
    EXPECT_TRUE(obj.has_value());
    did_create_2.Signal();

    will_destroy.Wait();
  }};

  EXPECT_OK(did_create_1.Wait());
  EXPECT_EQ(0, object_count_1->load());
  EXPECT_EQ(0, object_count_2->load());

  EXPECT_OK(loop().RunUntilIdle());
  EXPECT_EQ(1, object_count_1->load());
  EXPECT_EQ(0, object_count_2->load());

  will_create_2.Signal();
  EXPECT_OK(did_create_2.Wait());
  EXPECT_EQ(1, object_count_1->load());
  EXPECT_EQ(0, object_count_2->load());

  EXPECT_OK(loop().RunUntilIdle());
  EXPECT_EQ(0, object_count_1->load());
  EXPECT_EQ(1, object_count_2->load());

  will_destroy.Signal();
  t1.join();
  EXPECT_EQ(0, object_count_1->load());
  EXPECT_EQ(1, object_count_2->load());

  EXPECT_OK(loop().RunUntilIdle());
  EXPECT_EQ(0, object_count_1->load());
  EXPECT_EQ(0, object_count_2->load());
}

TEST_F(DispatcherBound, AsyncCall) {
  auto count = make_arc_atomic();
  async_patterns::DispatcherBound<ThreadAffine> obj{loop().dispatcher(), loop_thread_id(), count};
  EXPECT_OK(loop().RunUntilIdle());

  {
    auto result = make_arc_atomic();
    obj.AsyncCall(&ThreadAffine::Add, 1, 2, result);
    EXPECT_EQ(0, result->load());
    EXPECT_OK(loop().RunUntilIdle());
    EXPECT_EQ(3, result->load());
  }

  {
    std::unique_ptr p = std::make_unique<int>(42);
    obj.AsyncCall(&ThreadAffine::PassMoveOnly, std::move(p));
    EXPECT_EQ(nullptr, p);
    EXPECT_OK(loop().RunUntilIdle());
  }

  // Copy a |T| to a receiver that expects a |const T&|.
  {
    std::string s = "abc";
    obj.AsyncCall(&ThreadAffine::PassConstReference, s);
    EXPECT_EQ("abc", s);
    EXPECT_OK(loop().RunUntilIdle());
    EXPECT_EQ("abc", s);
  }

  // Move a |T| to a receiver that expects a |const T&|.
  {
    std::string s = "abc";
    obj.AsyncCall(&ThreadAffine::PassConstReference, std::move(s));
    EXPECT_EQ("", s);
    EXPECT_OK(loop().RunUntilIdle());
    EXPECT_EQ("", s);
  }

  // Move a |T| to a receiver that expects a |T|.
  {
    std::vector<int> v{1, 2, 3};
    obj.AsyncCall(&ThreadAffine::PassValue, std::move(v));
    EXPECT_EQ(0u, v.size());
    EXPECT_OK(loop().RunUntilIdle());
    EXPECT_EQ(0u, v.size());
  }

  // Copy a |T&| to a receiver that expects a |T|.
  {
    std::vector<int> v2{1, 2, 3};
    std::vector<int>& v2_ref = v2;
    obj.AsyncCall(&ThreadAffine::PassValue, v2_ref);
    EXPECT_EQ(3u, v2_ref.size());
    EXPECT_OK(loop().RunUntilIdle());
    EXPECT_EQ(3u, v2_ref.size());
  }

  // Pass-through a |T&| to a receiver that expects a |T&| is not supported.
#if 0
  {
    std::string s = "abc";
    std::string& s2 = s;
    obj.AsyncCall(&ThreadAffine::PassReference, s2);
    EXPECT_EQ("abc", s2);
    EXPECT_OK(loop().RunUntilIdle());
    EXPECT_EQ("", s2);
  }
#endif

  // Pointer arguments are not supported.
#if 0
  {
    std::string s = "abc";
    obj.AsyncCall(&ThreadAffine::PassPointer, &s);
    EXPECT_EQ("abc", s);
    EXPECT_OK(loop().RunUntilIdle());
    EXPECT_EQ("", s);
  }
#endif
}

TEST_F(DispatcherBound, PanicIfShutdown) {
  loop().Shutdown();
  async_patterns::DispatcherBound<ThreadAffine> obj;
  auto count = make_arc_atomic();
  ASSERT_DEATH(obj.emplace(loop().dispatcher(), loop_thread_id(), count),
               "The \\|async_dispatcher_t\\| was shut down before creating, making calls, or "
               "destroying an \\|async_patterns::DispatcherBound\\|. This is not allowed.");
}

TEST_F(DispatcherBound, DispatcherOutlivesDispatcherBound) {
  auto count = make_arc_atomic();
  {
    async_patterns::DispatcherBound<ThreadAffine> obj;
    obj.emplace(loop().dispatcher(), loop_thread_id(), count);
  }
  loop().RunUntilIdle();
  EXPECT_EQ(0, count->load());
}

}  // namespace
