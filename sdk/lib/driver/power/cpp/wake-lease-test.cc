// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async/default.h>
#include <lib/driver/power/cpp/wake-lease.h>
#include <lib/zx/clock.h>
#include <lib/zx/eventpair.h>
#include <lib/zx/time.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/syscalls/object.h>
#include <zircon/time.h>

#include <fbl/ref_ptr.h>
#include <gtest/gtest.h>
#include <src/lib/testing/loop_fixture/real_loop_fixture.h>
#include <src/storage/lib/vfs/cpp/service.h>
#include <src/storage/lib/vfs/cpp/synchronous_vfs.h>

#include "testing-common.h"

namespace power_lib_test {

class WakeLeaseTest : public gtest::RealLoopFixture {};

namespace {
// Prepares the resources needed to run the fake SAG server.
void PrepFakeSag(
    fbl::RefPtr<fs::Service>& sag,
    std::shared_ptr<fidl::ServerBindingGroup<fuchsia_power_system::ActivityGovernor>>& bindings,
    async::Loop& loop, std::shared_ptr<SystemActivityGovernor>& sag_server) {
  zx::event exec_opportunistic, wake_assertive;
  zx::event::create(0, &exec_opportunistic);
  zx::event::create(0, &wake_assertive);
  sag_server = std::make_shared<SystemActivityGovernor>(
      std::move(exec_opportunistic), std::move(wake_assertive), loop.dispatcher());

  bindings = std::make_shared<fidl::ServerBindingGroup<fuchsia_power_system::ActivityGovernor>>();
  sag = fbl::MakeRefCounted<fs::Service>(
      [&](fidl::ServerEnd<fuchsia_power_system::ActivityGovernor> chan) {
        bindings->AddBinding(loop.dispatcher(), std::move(chan), sag_server.get(),
                             fidl::kIgnoreBindingClosure);
        return ZX_OK;
      });
}

// Run an `ManualWakeLease` test with a fake SAG where the fake SAG and the
// client run on their own threads. `client_operations` will be run on the
// thread that owns the `ManualWakeLease` while `sag_operations` will be run on
// the fake SAG thread. These operations are run concurrently so if one needs
// to run before the other they must have internal synchronization. It is
// expect that `client_operations` will quit the `Loop` before it completes.
void DoManualWakeLeaseTest(
    const std::function<void(std::shared_ptr<fdf_power::ManualWakeLease>, async::Loop&)>&
        client_operations,
    const std::function<void(std::shared_ptr<SystemActivityGovernor>)>& sag_operations) {
  async::Loop server_loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  server_loop.StartThread("server-loop");

  async::Loop client_loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  client_loop.StartThread("client-loop");

  // The server needs to outlive the client, so create references that exist
  // until after the client's work concludes
  std::shared_ptr<SystemActivityGovernor> sag_server;
  std::shared_ptr<fidl::ServerBindingGroup<fuchsia_power_system::ActivityGovernor>> bindings;
  fbl::RefPtr<fs::Service> sag;

  async::PostTask(server_loop.dispatcher(), [&client_loop, &server_loop, &sag_server, &bindings,
                                             &sag, client_operations, sag_operations]() mutable {
    // First create SAG and related entities.
    PrepFakeSag(sag, bindings, server_loop, sag_server);

    // Create a channel connected to client and server.
    fidl::Endpoints<fuchsia_power_system::ActivityGovernor> sag_endpoints =
        fidl::Endpoints<fuchsia_power_system::ActivityGovernor>::Create();
    sag->ConnectService(sag_endpoints.server.TakeChannel());

    // Extract the channel from the client end, because passing a ClientEnd to
    // another thread causes problems with thread unsafe FIDL bindings.
    zx::channel client = sag_endpoints.client.TakeChannel();

    // Tell the client to do its work.
    async::PostTask(client_loop.dispatcher(), [&client_loop, &server_loop,
                                               client = std::move(client), &sag_server,
                                               client_operations, sag_operations]() mutable {
      std::shared_ptr<fdf_power::ManualWakeLease> op = std::make_shared<fdf_power::ManualWakeLease>(
          client_loop.dispatcher(), "test-operation",
          fidl::ClientEnd<fuchsia_power_system::ActivityGovernor>(std::move(client)));

      // We want to test what ManualWakeLease does while the system is resumed,
      // so have the server send the resume event.
      async::PostTask(server_loop.dispatcher(),
                      [&sag_server, sag_operations]() { sag_operations(sag_server); });

      // Trigger the start of the atomic operation.
      async::PostTask(client_loop.dispatcher(), [op, &client_loop, client_operations]() {
        client_operations(op, client_loop);
      });
    });
  });

  // The client will quit its loop after doing its work, so wait for it.
  client_loop.JoinThreads();

  // On the server thread, destroy the server objects.
  async::PostTask(server_loop.dispatcher(), [&sag_server, &bindings, &sag, &server_loop]() {
    // Destroy all the shared objects on the thread where they were created.
    sag.reset();
    bindings.reset();
    sag_server.reset();
    server_loop.Quit();
  });

  // Shut down the server thread.
  server_loop.JoinThreads();
}

// Waits for the ManualWakeLease to observe a suspension signal, then runs
// `do_after_suspend`.
void DoOperationAfterSuspend(
    const std::shared_ptr<fdf_power::ManualWakeLease>& op, async::Loop& loop,
    const std::function<void(const std::shared_ptr<fdf_power::ManualWakeLease>&, async::Loop&)>&
        do_after_suspend) {
  if (op->IsResumed()) {
    async::PostDelayedTask(
        loop.dispatcher(),
        [op, &loop, do_after_suspend]() { DoOperationAfterSuspend(op, loop, do_after_suspend); },
        zx::msec(100));
    return;
  };

  do_after_suspend(op, loop);
}

// Call |op->Start()| once |op| reports it considers the system resumed.
// After that, instruct |sag| to suspend. Concurrently call
// |DoOperationAfterSuspend|, passing |do_after_suspended| which means that
// |do_after_suspended| runs once |op| reports it considers the system
// suspended.
void StartOperationWhenResumedThenSuspend(
    const std::shared_ptr<fdf_power::ManualWakeLease>& op, async::Loop& client_loop,
    const std::shared_ptr<SystemActivityGovernor>& sag, async::Loop& server_loop,
    const std::function<void(std::shared_ptr<fdf_power::ManualWakeLease>, async::Loop&)>&
        do_after_suspended) {
  if (!op->IsResumed()) {
    async::PostDelayedTask(
        client_loop.dispatcher(),
        [op, &client_loop, sag, &server_loop, do_after_suspended]() {
          StartOperationWhenResumedThenSuspend(op, client_loop, sag, server_loop,
                                               do_after_suspended);
        },
        zx::msec(100));
    return;
  }

  EXPECT_FALSE(op->Start());
  async::PostTask(server_loop.dispatcher(), [sag]() { sag->SendSuspend(); });
  DoOperationAfterSuspend(op, client_loop, do_after_suspended);
}

void CheckLeaseAcquired(const std::shared_ptr<fdf_power::ManualWakeLease>& op, async::Loop& loop) {
  EXPECT_TRUE(op->End().is_ok());
  loop.Quit();
}

}  // namespace

// Create an ManualWakeLease and allow it to observe a resume signal. Then
// check that no actual lease is taken.
TEST_F(WakeLeaseTest, TestManualWakeLeaseWhenResumed) {
  const std::function<void(const std::shared_ptr<fdf_power::ManualWakeLease>&, async::Loop&)>
      test_func = [&test_func](const std::shared_ptr<fdf_power::ManualWakeLease>& op,
                               async::Loop& loop) {
        if (!op->IsResumed()) {
          async::PostDelayedTask(
              loop.dispatcher(), [op, &loop, &test_func]() { test_func(op, loop); }, zx::msec(100));
          return;
        }

        EXPECT_FALSE(op->Start());
        loop.Quit();
      };

  DoManualWakeLeaseTest(
      test_func, [](const std::shared_ptr<SystemActivityGovernor>& sag) { sag->SendResume(); });
}

// After the ManualWakeLease is created, have it observe a resume and then
// verify it works as expected when the operation starts and ends.
TEST_F(WakeLeaseTest, TestManualWakeLeaseStartAndEndAfterResumeIsObserved) {
  std::function<void(std::shared_ptr<fdf_power::ManualWakeLease>, async::Loop&)> test_func =
      [&test_func](const std::shared_ptr<fdf_power::ManualWakeLease>& op, async::Loop& loop) {
        // Wait for us to be in a resumed state so the atomic op obesrved the
        // system state change
        if (!op->IsResumed()) {
          async::PostDelayedTask(
              loop.dispatcher(), [op, &loop, &test_func]() { test_func(op, loop); }, zx::msec(100));
          return;
        }

        // Since the system is resumed we expect no lease to be taken
        EXPECT_FALSE(op->Start());
        // Since the system was resumed teh whole time, there should be no
        // lease to return when the operation ends.
        EXPECT_TRUE(op->End().is_error());
        loop.Quit();
      };

  DoManualWakeLeaseTest(
      test_func, [](const std::shared_ptr<SystemActivityGovernor>& sag) { sag->SendResume(); });
}

// Test ManualWakeLease when it starts while the system is suspended. Also
// check that duplicate `Start` calls result in taking only one lease.
TEST_F(WakeLeaseTest, TestManualWakeLeaseWhenSuspended) {
  std::function<void(std::shared_ptr<fdf_power::ManualWakeLease>, async::Loop&)> test_func =
      [](const std::shared_ptr<fdf_power::ManualWakeLease>& op, async::Loop& loop) {
        EXPECT_FALSE(op->IsResumed());
        EXPECT_TRUE(op->Start());
        EXPECT_FALSE(op->Start());
        loop.Quit();
      };
  DoManualWakeLeaseTest(test_func, [](const std::shared_ptr<SystemActivityGovernor>& sag) {});
}

// Checks that when we are suspended we can start an ManualWakeLease and then
// end it without error.
TEST_F(WakeLeaseTest, TestManualWakeLeaseStartAndEndWhileSuspended) {
  std::function<void(std::shared_ptr<fdf_power::ManualWakeLease>, async::Loop&)> test_func =
      [](const std::shared_ptr<fdf_power::ManualWakeLease>& op, async::Loop& loop) {
        EXPECT_FALSE(op->IsResumed());
        EXPECT_TRUE(op->Start());
        EXPECT_TRUE(op->End().is_ok());
        loop.Quit();
      };
  DoManualWakeLeaseTest(test_func, [](const std::shared_ptr<SystemActivityGovernor>& sag) {});
}

// Tests what happens happens when an ManualWakeLease observes a resume signal
// and then we start an ManualWakeLease. We expect no lease is taken. After
// verifying that, send a suspend signal which should trigger the
// ManualWakeLease to claim a wake lease. We then verify that a wake lease was
// actually taken.
TEST_F(WakeLeaseTest, TestManagedWakeLeaseWhenResumedThenSuspend) {
  async::Loop server_loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  server_loop.StartThread("server-loop");

  async::Loop client_loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  client_loop.StartThread("client-loop");

  // The server needs to outlive the client, so create references that exist
  // until after the client's work concludes
  std::shared_ptr<SystemActivityGovernor> sag_server;
  std::shared_ptr<fidl::ServerBindingGroup<fuchsia_power_system::ActivityGovernor>> bindings;
  fbl::RefPtr<fs::Service> sag;

  async::PostTask(server_loop.dispatcher(), [&client_loop, &server_loop, &sag_server, &bindings,
                                             &sag]() mutable {
    // First create SAG and related entities.
    PrepFakeSag(sag, bindings, server_loop, sag_server);

    // Create a channel connected to client and server.
    fidl::Endpoints<fuchsia_power_system::ActivityGovernor> sag_endpoints =
        fidl::Endpoints<fuchsia_power_system::ActivityGovernor>::Create();
    sag->ConnectService(sag_endpoints.server.TakeChannel());

    // Extract the channel from the client end, because passing a ClientEnd to
    // another thread causes problems with thread unsafe FIDL bindings.
    zx::channel client = sag_endpoints.client.TakeChannel();

    // Tell the client to do its work.
    async::PostTask(client_loop.dispatcher(), [&client_loop, &server_loop,
                                               client = std::move(client), &sag_server]() mutable {
      std::shared_ptr<fdf_power::ManualWakeLease> op = std::make_shared<fdf_power::ManualWakeLease>(
          client_loop.dispatcher(), "test-operation",
          fidl::ClientEnd<fuchsia_power_system::ActivityGovernor>(std::move(client)));

      // We want to test what ManualWakeLease does while the system is resumed,
      // so have the server send the resume event.
      async::PostTask(server_loop.dispatcher(), [&sag_server]() { sag_server->SendResume(); });

      // Trigger the start of the atomic operation.
      async::PostTask(client_loop.dispatcher(), [op, &client_loop, &server_loop, &sag_server]() {
        StartOperationWhenResumedThenSuspend(op, client_loop, sag_server, server_loop,
                                             CheckLeaseAcquired);
      });
    });
  });

  // The client will quit its loop after doing its work, so wait for it.
  client_loop.JoinThreads();

  // On the server thread, destroy the server objects.
  async::PostTask(server_loop.dispatcher(), [&sag_server, &bindings, &sag, &server_loop]() {
    // Destroy all the shared objects on the thread where they were created.
    sag.reset();
    bindings.reset();
    sag_server.reset();
    server_loop.Quit();
  });

  // Shut down the server thread.
  server_loop.JoinThreads();
}

// Verifies that a SharedWakeLeaseProvider and SharedWakeLease
// manage underlying resources as expected including that new
// SharedWakeLeases are created when appropriate and the underlying
// WakeLease object is preserved.
TEST_F(WakeLeaseTest, SharedWakeLeaseProviderTest) {
  async::Loop server_loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  server_loop.StartThread("server-loop");

  async::Loop client_loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  client_loop.StartThread("client-loop");

  // The server needs to outlive the client, so create references that exist
  // until after the client's work concludes
  std::shared_ptr<SystemActivityGovernor> sag_server;
  std::shared_ptr<fidl::ServerBindingGroup<fuchsia_power_system::ActivityGovernor>> bindings;
  fbl::RefPtr<fs::Service> sag;

  async::PostTask(server_loop.dispatcher(), [&client_loop, &server_loop, &sag_server, &bindings,
                                             &sag]() mutable {
    // First create SAG and related entities.
    PrepFakeSag(sag, bindings, server_loop, sag_server);

    // Create a channel connected to client and server.
    fidl::Endpoints<fuchsia_power_system::ActivityGovernor> sag_endpoints =
        fidl::Endpoints<fuchsia_power_system::ActivityGovernor>::Create();
    sag->ConnectService(sag_endpoints.server.TakeChannel());

    // Extract the channel from the client end, because passing a ClientEnd to
    // another thread causes problems with thread unsafe FIDL bindings.
    zx::channel client = sag_endpoints.client.TakeChannel();

    // Tell the client to do its work.
    async::PostTask(client_loop.dispatcher(), [&client_loop, client = std::move(client)]() mutable {
      std::shared_ptr<fdf_power::SharedWakeLeaseProvider> operation_provider =
          std::make_shared<fdf_power::SharedWakeLeaseProvider>(
              client_loop.dispatcher(), "test-operation",
              fidl::ClientEnd<fuchsia_power_system::ActivityGovernor>(std::move(client)));

      std::shared_ptr<fdf_power::SharedWakeLease> op1 = operation_provider->StartOperation();
      std::shared_ptr<fdf_power::SharedWakeLease> op2 = operation_provider->StartOperation();

      EXPECT_EQ(op1, op2);

      // Get a raw pointer to the current SharedWakeLease to check
      // against a different pointer later which will help us prove we dropped
      // the SharedWakeLease after all strong pointers to it were
      // dropped.
      fdf_power::SharedWakeLease* old_addr = op1.get();
      std::shared_ptr<fdf_power::TimeoutWakeLease> first_lease = op1->GetWakeLease();
      op1.reset();
      op2.reset();

      // It should be that we currently have no system wake lease since there
      // are no valid SharedWakeLeases.
      EXPECT_TRUE(first_lease->GetWakeLeaseCopy().is_error());

      // Start a new operation, which should create a new
      // SharedWakeLease.
      std::shared_ptr<fdf_power::SharedWakeLease> op3 = operation_provider->StartOperation();
      EXPECT_NE(old_addr, op3.get());

      // The fdf_power::WakeLease should be the same, even though the
      // SharedWakeLease changed.
      std::shared_ptr<fdf_power::TimeoutWakeLease> second_lease = op3->GetWakeLease();
      EXPECT_EQ(first_lease, second_lease);

      client_loop.Quit();
    });
  });

  // The client will quit its loop after doing its work, so wait for it.
  client_loop.JoinThreads();

  // On the server thread, destroy the server objects.
  async::PostTask(server_loop.dispatcher(), [&sag_server, &bindings, &sag, &server_loop]() {
    // Destroy all the shared objects on the thread where they were created.
    sag.reset();
    bindings.reset();
    sag_server.reset();
    server_loop.Quit();
  });

  // Shut down the server thread.
  server_loop.JoinThreads();
}

TEST_F(WakeLeaseTest, TestWakeLeaseTimeouts) {
  fidl::Endpoints<fuchsia_power_system::ActivityGovernor> endpoints =
      fidl::Endpoints<fuchsia_power_system::ActivityGovernor>();
  // We're exploiting properties of the WakeLease implementation here, in
  // particular that it only talks to SAG, which we aren't faking, if it
  // needs a lease, not if it already has one. This means that WakeLease can
  // hold the contradictory thoughts in its head that we have a wake lease,
  // because it was deposited, but we are also suspended, because it hasn't
  // been told otherwise. Neat!

  fdf_power::TimeoutWakeLease test_lease = fdf_power::TimeoutWakeLease(
      async_get_default_dispatcher(), "test-lease", std::move(endpoints.client));

  EXPECT_EQ(test_lease.GetNextTimeout(), ZX_TIME_INFINITE);
  zx::eventpair h1, h2, h3;
  zx::eventpair::create(0, &h1, &h2);
  h1.duplicate(ZX_RIGHT_SAME_RIGHTS, &h3);
  zx::time expire_time = zx::clock::get_monotonic() + zx::hour(1);
  test_lease.DepositWakeLease(std::move(h3), expire_time);
  EXPECT_EQ(test_lease.GetNextTimeout(), expire_time.get());

  h1.duplicate(ZX_RIGHT_SAME_RIGHTS, &h3);
  zx::time expire_time2 = expire_time - zx::min(1);
  test_lease.DepositWakeLease(std::move(h3), expire_time2);
  EXPECT_EQ(test_lease.GetNextTimeout(), expire_time.get());

  h1.duplicate(ZX_RIGHT_SAME_RIGHTS, &h3);
  zx::time expire_time3 = expire_time + zx::min(1);
  test_lease.DepositWakeLease(std::move(h3), expire_time3);
  EXPECT_EQ(test_lease.GetNextTimeout(), expire_time3.get());

  // Acquire a wake lease, specifying a timeout. The time the timeout is set
  // for should be between the time before the call and the time after the
  // call, plus the timeout.
  zx::duration timeout = zx::min(10);
  zx::time before = zx::clock::get_monotonic();
  test_lease.AcquireWakeLease(timeout);
  zx::time after = zx::clock::get_monotonic();
  EXPECT_GE(test_lease.GetNextTimeout(), (before + timeout).get());
  EXPECT_LE(test_lease.GetNextTimeout(), (after + timeout).get());

  // Acquire a new wake lease, but with a shorter timeout, which we expect to
  // be changed.
  timeout = zx::min(5);
  before = zx::clock::get_monotonic();
  test_lease.AcquireWakeLease(timeout);
  after = zx::clock::get_monotonic();
  zx_time_t next_timeout = test_lease.GetNextTimeout();
  EXPECT_GE(next_timeout, (before + timeout).get());
  EXPECT_LE(next_timeout, (after + timeout).get());

  // Since the wake lease has never heard anything about whether the system is
  // suspended or resumed, it assumes we're suspended, tell it we're resumed
  test_lease.SetSuspended(false);

  // Now tell it we need to keep the system awake until a given time
  before = zx::clock::get_monotonic();
  EXPECT_FALSE(test_lease.HandleInterrupt(timeout));
  // Add a little slack here because we get some small delays later on
  // since the implementation translates from an absolute time to an offset
  // and then posts the task at that offset a bit after calculating it. Local
  // testing indicates tens of microseconds of skew.
  after = zx::clock::get_monotonic() + zx::sec(2);

  // Right now we expect no reclamation task for the eventpair because the wake
  // lease considers the system awake.
  EXPECT_EQ(test_lease.GetNextTimeout(), next_timeout);

  // Now, let's "suspend", which should trigger a lease, and therefore a
  // task to reclaim it.
  test_lease.SetSuspended(true);
  next_timeout = test_lease.GetNextTimeout();
  EXPECT_GE(next_timeout, (before + timeout).get());
  EXPECT_LE(next_timeout, (after + timeout).get());
}
}  // namespace power_lib_test
