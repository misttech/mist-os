// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/starnix_sync/port_event.h>
#include <lib/unittest/unittest.h>

#include <object/event_dispatcher.h>
#include <object/job_dispatcher.h>
#include <object/process_dispatcher.h>
#include <object/thread_dispatcher.h>
#include <object/vm_address_region_dispatcher.h>

namespace unit_testing {

namespace {

#if 0
KernelHandle<ThreadDispatcher> CreateThreadDispatcher() {
  KernelHandle<JobDispatcher> job;
  zx_rights_t job_rights;
  auto status = JobDispatcher::Create(0u, GetRootJobDispatcher(), &job, &job_rights);
  ZX_ASSERT(status == ZX_OK);

  KernelHandle<ProcessDispatcher> process;
  KernelHandle<VmAddressRegionDispatcher> vmar;
  zx_rights_t process_rights;
  zx_rights_t vmar_rights;
  status = ProcessDispatcher::Create(job.dispatcher(), "k-ut-p1", 0u, &process, &process_rights,
                                     &vmar, &vmar_rights);
  ZX_ASSERT(status == ZX_OK);

  KernelHandle<ThreadDispatcher> thread;
  zx_rights_t thread_rights;
  status = ThreadDispatcher::Create(process.dispatcher(), 0u, "k-ut-t1", &thread, &thread_rights);
  ZX_ASSERT(status == ZX_OK);

  status = thread.dispatcher()->Initialize();
  ZX_ASSERT(status == ZX_OK);

  return thread;
}

bool test_signal_and_wait_block() {
  BEGIN_TEST;

  constexpr uint64_t KEY = 1;
  constexpr zx_signals_t ASSERTED_SIGNAL = ZX_USER_SIGNAL_0;

  fbl::AllocChecker ac;
  auto event = fbl::MakeRefCountedChecked<starnix_sync::PortEvent>(&ac);
  ASSERT(ac.check());

  KernelHandle<EventDispatcher> object;
  zx_rights_t rights;
  zx_status_t result = EventDispatcher::Create(0, &object, &rights);
  if (result != ZX_OK) {
    return result;
  }

  ASSERT_TRUE(event->ObjectWaitAsync(object.dispatcher(), KEY, ASSERTED_SIGNAL, ZX_WAIT_ASYNC_ONCE)
                  .is_ok());

  auto thread = Thread::Create(
      "event waiter",
      [](void *arg) -> int {
        BEGIN_TEST;
        printf("event waiter\n");

        auto event = static_cast<starnix_sync::PortEvent *>(arg);
        starnix_sync::PortWaitResult result = event->Wait(ZX_TIME_INFINITE);
        ASSERT_TRUE(result.IsSignal());
        ASSERT_EQ(1u, result.GetSignal().key);
        ASSERT_EQ(ZX_USER_SIGNAL_0, result.GetSignal().observed);
        END_TEST;
      },
      event.get(), DEFAULT_PRIORITY);

  auto td = CreateThreadDispatcher();
  thread->SetUsermodeThread(td.dispatcher());

  thread->Resume();

  // ASSERT_EQ(object.dispatcher()->user_signal_self(ZX_SIGNAL_NONE, ASSERTED_SIGNAL), ZX_OK);

  // ASSERT_EQ(object.signal(0, ASSERTED_SIGNAL), ZX_OK);
  int retval;
  ASSERT_EQ(ZX_OK, thread->Join(&retval, ZX_TIME_INFINITE));
  ASSERT_EQ(all_ok, retval);

  END_TEST;
}


bool test_signal_then_wait_nonblock() {
  BEGIN_TEST;

  constexpr uint64_t KEY = 2;
  constexpr zx_signals_t ASSERTED_SIGNAL = ZX_USER_SIGNAL_1;

  fbl::AllocChecker ac;
  auto event = fbl::MakeRefCountedChecked<starnix_sync::PortEvent>(&ac);
  ASSERT(ac.check());

  KernelHandle<EventDispatcher> object;
  zx_rights_t rights;
  zx_status_t status = EventDispatcher::Create(0, &object, &rights);
  ASSERT_EQ(ZX_OK, status);

  ASSERT_TRUE(event->ObjectWaitAsync(object.dispatcher(), KEY, ASSERTED_SIGNAL, ZX_WAIT_ASYNC_ONCE)
                  .is_ok());

  ASSERT_EQ(object.dispatcher()->user_signal_self(ZX_SIGNAL_NONE, ASSERTED_SIGNAL), ZX_OK);

  starnix_sync::PortWaitResult result = event->Wait(ZX_TIME_INFINITE_PAST);
  ASSERT_TRUE(result.IsSignal());
  ASSERT_EQ(KEY, result.GetSignal().key);
  ASSERT_EQ(ASSERTED_SIGNAL, result.GetSignal().observed);
  
  END_TEST;
}
#endif

}  // namespace

}  // namespace unit_testing

//UNITTEST_START_TESTCASE(starnix_sync_port)
// UNITTEST("test signal and wait block", unit_testing::test_signal_and_wait_block)
// UNITTEST("test signal then wait nonblock", unit_testing::test_signal_then_wait_nonblock)
//UNITTEST_END_TESTCASE(starnix_sync_port, "starnix_sync_port", "Tests for starnix port sync")
