// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/memory/weak_ptr.h"

#include <lib/unittest/unittest.h>

#include <utility>

namespace unit_testing {
namespace {

using mtl::WeakPtr;
using mtl::WeakPtrFactory;

bool Basic() {
  BEGIN_TEST;

  int data = 0;
  WeakPtrFactory<int> factory(&data);
  WeakPtr<int> ptr = factory.GetWeakPtr();
  EXPECT_EQ(&data, ptr.get());

  END_TEST;
}

bool CopyConstruction() {
  BEGIN_TEST;

  int data = 0;
  WeakPtrFactory<int> factory(&data);
  WeakPtr<int> ptr = factory.GetWeakPtr();
  WeakPtr<int> ptr2(ptr);
  EXPECT_EQ(&data, ptr.get());
  EXPECT_EQ(&data, ptr2.get());

  END_TEST;
}

bool MoveConstruction() {
  BEGIN_TEST;

  int data = 0;
  WeakPtrFactory<int> factory(&data);
  WeakPtr<int> ptr = factory.GetWeakPtr();
  WeakPtr<int> ptr2(std::move(ptr));
  EXPECT_EQ(nullptr, ptr.get());
  EXPECT_EQ(&data, ptr2.get());

  END_TEST;
}

bool CopyAssignment() {
  BEGIN_TEST;

  int data = 0;
  WeakPtrFactory<int> factory(&data);
  WeakPtr<int> ptr = factory.GetWeakPtr();
  WeakPtr<int> ptr2;
  EXPECT_EQ(nullptr, ptr2.get());
  ptr2 = ptr;
  EXPECT_EQ(&data, ptr.get());
  EXPECT_EQ(&data, ptr2.get());

  END_TEST;
}

bool MoveAssignment() {
  BEGIN_TEST;

  int data = 0;
  WeakPtrFactory<int> factory(&data);
  WeakPtr<int> ptr = factory.GetWeakPtr();
  WeakPtr<int> ptr2;
  EXPECT_EQ(nullptr, ptr2.get());
  ptr2 = std::move(ptr);
  EXPECT_EQ(nullptr, ptr.get());
  EXPECT_EQ(&data, ptr2.get());

  END_TEST;
}

bool NullConstruction() {
  BEGIN_TEST;

  WeakPtr<int> ptr = nullptr;
  EXPECT_FALSE(ptr);
  EXPECT_EQ(nullptr, ptr.get());

  END_TEST;
}

bool NullAssignment() {
  BEGIN_TEST;

  int data = 0;
  WeakPtrFactory<int> factory(&data);
  WeakPtr<int> ptr = factory.GetWeakPtr();
  ptr = nullptr;
  EXPECT_FALSE(ptr);
  EXPECT_EQ(nullptr, ptr.get());

  END_TEST;
}

bool Testable() {
  BEGIN_TEST;

  int data = 0;
  WeakPtrFactory<int> factory(&data);
  WeakPtr<int> ptr;
  EXPECT_EQ(nullptr, ptr.get());
  EXPECT_FALSE(ptr);
  ptr = factory.GetWeakPtr();
  EXPECT_EQ(&data, ptr.get());
  EXPECT_TRUE(ptr);

  END_TEST;
}

bool OutOfScope() {
  BEGIN_TEST;

  WeakPtr<int> ptr;
  EXPECT_EQ(nullptr, ptr.get());
  {
    int data = 0;
    WeakPtrFactory<int> factory(&data);
    ptr = factory.GetWeakPtr();
  }
  EXPECT_EQ(nullptr, ptr.get());

  END_TEST;
}

bool Multiple() {
  BEGIN_TEST;

  WeakPtr<int> a;
  WeakPtr<int> b;
  {
    int data = 0;
    WeakPtrFactory<int> factory(&data);
    a = factory.GetWeakPtr();
    b = factory.GetWeakPtr();
    EXPECT_EQ(&data, a.get());
    EXPECT_EQ(&data, b.get());
  }
  EXPECT_EQ(nullptr, a.get());
  EXPECT_EQ(nullptr, b.get());

  END_TEST;
}

bool MultipleStaged() {
  BEGIN_TEST;

  WeakPtr<int> a;
  {
    int data = 0;
    WeakPtrFactory<int> factory(&data);
    a = factory.GetWeakPtr();
    {
      WeakPtr<int> b = factory.GetWeakPtr();
    }
    EXPECT_NE(a.get(), nullptr);
  }
  EXPECT_EQ(nullptr, a.get());

  END_TEST;
}

struct Base {
  double member = 0.;
};
struct Derived : public Base {};

bool Dereference() {
  BEGIN_TEST;

  Base data;
  data.member = 123456.;
  WeakPtrFactory<Base> factory(&data);
  WeakPtr<Base> ptr = factory.GetWeakPtr();
  EXPECT_EQ(&data, ptr.get());
  EXPECT_EQ(data.member, (*ptr).member);
  EXPECT_EQ(data.member, ptr->member);

  END_TEST;
}

bool UpcastCopyConstruction() {
  BEGIN_TEST;

  Derived data;
  WeakPtrFactory<Derived> factory(&data);
  WeakPtr<Derived> ptr = factory.GetWeakPtr();
  WeakPtr<Base> ptr2(ptr);
  EXPECT_EQ(&data, ptr.get());
  EXPECT_EQ(&data, ptr2.get());

  END_TEST;
}

bool UpcastMoveConstruction() {
  BEGIN_TEST;

  Derived data;
  WeakPtrFactory<Derived> factory(&data);
  WeakPtr<Derived> ptr = factory.GetWeakPtr();
  WeakPtr<Base> ptr2(std::move(ptr));
  EXPECT_EQ(nullptr, ptr.get());
  EXPECT_EQ(&data, ptr2.get());

  END_TEST;
}

bool UpcastCopyAssignment() {
  BEGIN_TEST;

  Derived data;
  WeakPtrFactory<Derived> factory(&data);
  WeakPtr<Derived> ptr = factory.GetWeakPtr();
  WeakPtr<Base> ptr2;
  EXPECT_EQ(nullptr, ptr2.get());
  ptr2 = ptr;
  EXPECT_EQ(&data, ptr.get());
  EXPECT_EQ(&data, ptr2.get());

  END_TEST;
}

bool UpcastMoveAssignment() {
  BEGIN_TEST;

  Derived data;
  WeakPtrFactory<Derived> factory(&data);
  WeakPtr<Derived> ptr = factory.GetWeakPtr();
  WeakPtr<Base> ptr2;
  EXPECT_EQ(nullptr, ptr2.get());
  ptr2 = std::move(ptr);
  EXPECT_EQ(nullptr, ptr.get());
  EXPECT_EQ(&data, ptr2.get());

  END_TEST;
}

bool InvalidateWeakPtrs() {
  BEGIN_TEST;

  int data = 0;
  WeakPtrFactory<int> factory(&data);
  WeakPtr<int> ptr = factory.GetWeakPtr();
  EXPECT_EQ(&data, ptr.get());
  EXPECT_TRUE(factory.HasWeakPtrs());
  factory.InvalidateWeakPtrs();
  EXPECT_EQ(nullptr, ptr.get());
  EXPECT_FALSE(factory.HasWeakPtrs());

  // Test that the factory can create new weak pointers after a
  // |InvalidateWeakPtrs()| call, and that they remain valid until the next
  // |InvalidateWeakPtrs()| call.
  WeakPtr<int> ptr2 = factory.GetWeakPtr();
  EXPECT_EQ(&data, ptr2.get());
  EXPECT_TRUE(factory.HasWeakPtrs());
  factory.InvalidateWeakPtrs();
  EXPECT_EQ(nullptr, ptr2.get());
  EXPECT_FALSE(factory.HasWeakPtrs());

  END_TEST;
}

bool HasWeakPtrs() {
  BEGIN_TEST;

  int data = 0;
  WeakPtrFactory<int> factory(&data);
  {
    WeakPtr<int> ptr = factory.GetWeakPtr();
    EXPECT_TRUE(factory.HasWeakPtrs());
  }
  EXPECT_FALSE(factory.HasWeakPtrs());

  END_TEST;
}

// TODO(vtl): Copy/convert the various threaded tests from Chromium's
// //base/memory/weak_ptr_unittest.cc.

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(mistos_memory_weakptr)
UNITTEST("Basic", unit_testing::Basic)
UNITTEST("CopyConstruction", unit_testing::CopyConstruction)
UNITTEST("MoveConstruction", unit_testing::MoveConstruction)
UNITTEST("CopyAssignment", unit_testing::CopyAssignment)
UNITTEST("MoveAssignment", unit_testing::MoveAssignment)
UNITTEST("NullConstruction", unit_testing::NullConstruction)
UNITTEST("NullAssignment", unit_testing::NullAssignment)
UNITTEST("Testable", unit_testing::Testable)
UNITTEST("OutOfScope", unit_testing::OutOfScope)
UNITTEST("Multiple", unit_testing::Multiple)
UNITTEST("MultipleStaged", unit_testing::MultipleStaged)
UNITTEST("Dereference", unit_testing::Dereference)
UNITTEST("UpcastCopyConstruction", unit_testing::UpcastCopyConstruction)
UNITTEST("UpcastMoveConstruction", unit_testing::UpcastMoveConstruction)
UNITTEST("UpcastCopyAssignment", unit_testing::UpcastCopyAssignment)
UNITTEST("UpcastMoveAssignment", unit_testing::UpcastMoveAssignment)
UNITTEST("InvalidateWeakPtrs", unit_testing::InvalidateWeakPtrs)
UNITTEST("HasWeakPtrs", unit_testing::HasWeakPtrs)
UNITTEST_END_TESTCASE(mistos_memory_weakptr, "mistos_memory_weakptr", "Tests MTL WeakPtr")
