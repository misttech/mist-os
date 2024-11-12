// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/util/weak_wrapper.h>
#include <lib/unittest/unittest.h>

#include <fbl/alloc_checker.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_counted_upgradeable.h>
#include <fbl/ref_ptr.h>

namespace unit_testing {
namespace {

class Obj : public fbl::RefCountedUpgradeable<Obj> {};

bool empty() {
  BEGIN_TEST;
  util::WeakPtr<Obj> weak;
  ASSERT_FALSE(weak.Lock());

  END_TEST;
}

bool lock() {
  BEGIN_TEST;
  fbl::AllocChecker ac;
  fbl::RefPtr<Obj> obj = fbl::MakeRefCountedChecked<Obj>(&ac);
  ASSERT(ac.check());

  {
    util::WeakPtr<Obj> weak(obj.get());
    ASSERT_TRUE(obj == weak.Lock());
  }
  END_TEST;
}

bool after() {
  BEGIN_TEST;

  util::WeakPtr<Obj> weak;
  {
    fbl::AllocChecker ac;
    fbl::RefPtr<Obj> obj = fbl::MakeRefCountedChecked<Obj>(&ac);
    ASSERT(ac.check());
    weak = util::WeakPtr<Obj>(obj.get());
    ASSERT_TRUE(obj == weak.Lock());
  }

  ASSERT_FALSE(weak.Lock());
  END_TEST;
}

#if 0
TEST(WeakPtr, Compare) {
  using ObjPtr = fbl::RefPtr<Obj>;
  using WeakObjPtr = util::WeakPtr<Obj>;

  fbl::AllocChecker ac;
  ObjPtr ptr1 = fbl::MakeRefCountedChecked<Obj>(&ac);
  ASSERT_TRUE(ac.check());
  WeakObjPtr wptr1(ptr1.get());

  ObjPtr ptr2 = fbl::MakeRefCountedChecked<Obj>(&ac);
  ASSERT_TRUE(ac.check());
  WeakObjPtr wptr2(ptr2.get());

  WeakObjPtr also_wptr1 = wptr1;
  WeakObjPtr null_ref_ptr;

  // TODO (Herrera) Fix double lock
  // EXPECT_TRUE(wptr1 == wptr1);
  // EXPECT_FALSE(wptr1 != wptr1);

  EXPECT_FALSE(wptr1 == wptr2);
  EXPECT_TRUE(wptr1 != wptr2);

  EXPECT_TRUE(wptr1 == also_wptr1);
  EXPECT_FALSE(wptr1 != also_wptr1);

  EXPECT_TRUE(wptr1 != null_ref_ptr);
  EXPECT_TRUE(wptr1 != nullptr);
  EXPECT_TRUE(nullptr != wptr1);
  EXPECT_FALSE(wptr1 == null_ref_ptr);
  EXPECT_FALSE(wptr1 == nullptr);
  EXPECT_FALSE(nullptr == wptr1);

  EXPECT_TRUE(null_ref_ptr == nullptr);
  EXPECT_FALSE(null_ref_ptr != nullptr);
  EXPECT_TRUE(nullptr == null_ref_ptr);
  EXPECT_FALSE(nullptr != null_ref_ptr);
}

TEST(WeakPtr, MoveAssign) {
  using ObjPtr = fbl::RefPtr<Obj>;
  using WeakObjPtr = util::WeakPtr<Obj>;

  Obj *obj1, *obj2;

  fbl::AllocChecker ac;
  obj1 = new (&ac) Obj();
  ASSERT_TRUE(ac.check());
  obj2 = new (&ac) Obj();
  ASSERT_TRUE(ac.check());

  ObjPtr ptr1 = fbl::AdoptRef<Obj>(obj1);
  ObjPtr ptr2 = fbl::AdoptRef<Obj>(obj2);

  WeakObjPtr wptr1(ptr1.get());
  WeakObjPtr wptr2 = WeakObjPtr(ptr2.get());

  EXPECT_NE(wptr1.get(), wptr2.get());
  EXPECT_NOT_NULL(wptr1);
  EXPECT_NOT_NULL(wptr2);
  wptr1 = std::move(wptr2);
  EXPECT_EQ(obj2, wptr1.get());
  EXPECT_EQ(ptr2, wptr1.Lock());
  EXPECT_NULL(wptr2);

  // Test self-assignment
#ifdef __clang__
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wself-move"
#endif
  wptr1 = std::move(wptr1);
#ifdef __clang__
#pragma clang diagnostic pop
#endif

  EXPECT_EQ(obj2, wptr1.get());
}
#endif

}  // namespace

}  // namespace unit_testing

UNITTEST_START_TESTCASE(mistos_weak_wrapper)
UNITTEST("empty", unit_testing::empty)
UNITTEST("lock", unit_testing::lock)
UNITTEST("after", unit_testing::after)
UNITTEST_END_TESTCASE(mistos_weak_wrapper, "mistos_weak_wrapper", "Tests Weak Ptr")
