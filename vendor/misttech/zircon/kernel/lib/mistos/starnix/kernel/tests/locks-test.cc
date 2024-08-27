// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/sync/locks.h>

#include <fbl/mutex.h>
#include <zxtest/zxtest.h>

#include "fbl/ref_counted.h"
#include "fbl/ref_ptr.h"

namespace {

using namespace starnix;

struct Data {
  int val;
};

struct DataRefPtr : public fbl::RefCounted<DataRefPtr> {
  int val;
};

TEST(Locks, MutexRaw) {
  StarnixMutex<Data> m(std::move(Data()));
  m.Lock()->val = 42;
  ASSERT_EQ(42, m.Lock()->val);
}

TEST(Locks, MutexRefPtr) {
  fbl::AllocChecker ac;
  fbl::RefPtr<DataRefPtr> obj = fbl::MakeRefCountedChecked<DataRefPtr>(&ac);
  ASSERT(ac.check());

  StarnixMutex<fbl::RefPtr<DataRefPtr>> m(std::move(obj));
  m.Lock()->get()->val = 42;
  ASSERT_EQ(42, m.Lock()->get()->val);
}

TEST(Locks, RwLock) {
  RwLock<Data> m(std::move(Data()));
  m.Write()->val = 42;
  ASSERT_EQ(42, m.Read()->val);
}

TEST(Locks, RwLockRefPtr) {
  fbl::AllocChecker ac;
  fbl::RefPtr<DataRefPtr> obj = fbl::MakeRefCountedChecked<DataRefPtr>(&ac);
  ASSERT(ac.check());

  RwLock<fbl::RefPtr<DataRefPtr>> m(std::move(obj));
  m.Write()->get()->val = 42;

  {
    RwLockGuard<fbl::RefPtr<DataRefPtr>, BrwLockPi::Reader> reader_ = m.Read();
    ASSERT_EQ(42, reader_->get()->val);
  }
}

}  // namespace
