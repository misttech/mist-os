// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/starnix_sync/locks.h>
#include <lib/unittest/unittest.h>

#include <fbl/mutex.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>

#include <ktl/enforce.h>

namespace unit_testing {

using namespace starnix_sync;

struct Data {
  int val;
};

struct DataRefPtr : public fbl::RefCounted<DataRefPtr> {
  int val;
};

bool test_mutex_raw() {
  BEGIN_TEST;
  StarnixMutex<Data> m(ktl::move(Data()));
  m.Lock()->val = 42;
  ASSERT_EQ(42, m.Lock()->val);
  END_TEST;
}

bool test_mutex_ref_ptr() {
  BEGIN_TEST;
  fbl::AllocChecker ac;
  fbl::RefPtr<DataRefPtr> obj = fbl::MakeRefCountedChecked<DataRefPtr>(&ac);
  ASSERT(ac.check());

  StarnixMutex<fbl::RefPtr<DataRefPtr>> m(ktl::move(obj));
  m.Lock()->get()->val = 42;
  ASSERT_EQ(42, m.Lock()->get()->val);
  END_TEST;
}

bool test_rwlock() {
  BEGIN_TEST;
  RwLock<Data> m(ktl::move(Data()));
  m.Write()->val = 42;
  ASSERT_EQ(42, m.Read()->val);
  END_TEST;
}

bool test_rwlock_ref_ptr() {
  BEGIN_TEST;
  fbl::AllocChecker ac;
  fbl::RefPtr<DataRefPtr> obj = fbl::MakeRefCountedChecked<DataRefPtr>(&ac);
  ASSERT(ac.check());

  RwLock<fbl::RefPtr<DataRefPtr>> m(ktl::move(obj));
  m.Write()->get()->val = 42;

  {
    RwLockGuard<fbl::RefPtr<DataRefPtr>, BrwLockPi::Reader> reader = m.Read();
    ASSERT_EQ(42, reader->get()->val);
  }
  END_TEST;
}

void do_write(RwLock<fbl::RefPtr<DataRefPtr>>::RwLockWriteGuard wg) { wg->get()->val = 42; }

bool test_rwlock_guard_move_write() {
  BEGIN_TEST;
  fbl::AllocChecker ac;
  fbl::RefPtr<DataRefPtr> obj = fbl::MakeRefCountedChecked<DataRefPtr>(&ac);
  ASSERT(ac.check());

  RwLock<fbl::RefPtr<DataRefPtr>> m(ktl::move(obj));
  m.Write()->get()->val = 0;

  {
    RwLock<fbl::RefPtr<DataRefPtr>>::RwLockWriteGuard wg = m.Write();
    do_write(ktl::move(wg));
  }

  ASSERT_EQ(42, m.Read()->get()->val);
  END_TEST;
}

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_sync)
UNITTEST("test mutex raw", unit_testing::test_mutex_raw)
UNITTEST("test mutex ref ptr", unit_testing::test_mutex_ref_ptr)
UNITTEST("test rwlock", unit_testing::test_rwlock)
UNITTEST("test rwlock ref ptr", unit_testing::test_rwlock_ref_ptr)
UNITTEST("test rwlock guard move write", unit_testing::test_rwlock_guard_move_write)
UNITTEST_END_TESTCASE(starnix_sync, "starnix_sync", "Tests for starnix sync")
