// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

#include <gtest/gtest.h>

#include "src/starnix/tests/syscalls/test_helper.h"

namespace {
test_helper::SignalMaskHelper sigmaskHelper = test_helper::SignalMaskHelper();
}  // namespace

class WaitpidFlagsTest : public testing::Test {
 protected:
  void SetUp() override {
    sigmaskHelper.blockSignal(SIGUSR1);
    errno = 0;
  }

  void TearDown() override { sigmaskHelper.restoreSigmask(); }
};

TEST_F(WaitpidFlagsTest, waitpidWithNoFlagsFailsWhenChildSendsNoSignal) {
  unsigned int waitFlag = 0;
  int cloneFlags = 0;
  test_helper::waitForChildFails(waitFlag, cloneFlags, test_helper::CloneHelper::sleep_1sec,
                                 test_helper::CloneHelper::doNothing);
  test_helper::waitForChildFails(waitFlag, cloneFlags, test_helper::CloneHelper::doNothing,
                                 test_helper::CloneHelper::sleep_1sec);
}

TEST_F(WaitpidFlagsTest, waitpidWithCloneFlagSucceedsWhenChildSendsNoSignal) {
  unsigned int waitFlag = __WCLONE;
  int cloneFlags = 0;
  test_helper::waitForChildSucceeds(waitFlag, cloneFlags, test_helper::CloneHelper::sleep_1sec,
                                    test_helper::CloneHelper::doNothing);
  test_helper::waitForChildSucceeds(waitFlag, cloneFlags, test_helper::CloneHelper::doNothing,
                                    test_helper::CloneHelper::sleep_1sec);
}

TEST_F(WaitpidFlagsTest, waitpidWithWallFlagSucceedsWhenChildSendsNoSignal) {
  unsigned int waitFlag = __WALL;
  int cloneFlags = 0;
  test_helper::waitForChildSucceeds(waitFlag, cloneFlags, test_helper::CloneHelper::sleep_1sec,
                                    test_helper::CloneHelper::doNothing);
  test_helper::waitForChildSucceeds(waitFlag, cloneFlags, test_helper::CloneHelper::doNothing,
                                    test_helper::CloneHelper::sleep_1sec);
}

TEST_F(WaitpidFlagsTest, waitpidWithNoFlagsFailsWhenChildSendsSIGUSR1) {
  unsigned int waitFlag = 0;
  int cloneFlags = SIGUSR1;
  test_helper::waitForChildFails(waitFlag, cloneFlags, test_helper::CloneHelper::sleep_1sec,
                                 test_helper::CloneHelper::doNothing);
  sigmaskHelper.waitForSignal(SIGUSR1);
  test_helper::waitForChildFails(waitFlag, cloneFlags, test_helper::CloneHelper::doNothing,
                                 test_helper::CloneHelper::sleep_1sec);
  sigmaskHelper.waitForSignal(SIGUSR1);
}

TEST_F(WaitpidFlagsTest, waipidWithCloneFlagSucceedsWhenChildSendsSIGUSR1) {
  unsigned int waitFlag = __WCLONE;
  int cloneFlags = SIGUSR1;
  test_helper::waitForChildSucceeds(waitFlag, cloneFlags, test_helper::CloneHelper::sleep_1sec,
                                    test_helper::CloneHelper::doNothing);
  sigmaskHelper.waitForSignal(SIGUSR1);
  test_helper::waitForChildSucceeds(waitFlag, cloneFlags, test_helper::CloneHelper::doNothing,
                                    test_helper::CloneHelper::sleep_1sec);
  sigmaskHelper.waitForSignal(SIGUSR1);
}

TEST_F(WaitpidFlagsTest, waipidWithWallFlagSucceedsWhenChildSendsSIGUSR1) {
  unsigned int waitFlag = __WALL;
  int cloneFlags = SIGUSR1;
  test_helper::waitForChildSucceeds(waitFlag, cloneFlags, test_helper::CloneHelper::sleep_1sec,
                                    test_helper::CloneHelper::doNothing);
  sigmaskHelper.waitForSignal(SIGUSR1);
  test_helper::waitForChildSucceeds(waitFlag, cloneFlags, test_helper::CloneHelper::doNothing,
                                    test_helper::CloneHelper::sleep_1sec);
  sigmaskHelper.waitForSignal(SIGUSR1);
}

TEST_F(WaitpidFlagsTest, waitpidWithNoFlagsSucceedsWhenChildSendsSIGCHLD) {
  unsigned int waitFlag = 0;
  int cloneFlags = SIGCHLD;
  test_helper::waitForChildSucceeds(waitFlag, cloneFlags, test_helper::CloneHelper::sleep_1sec,
                                    test_helper::CloneHelper::doNothing);
  test_helper::waitForChildSucceeds(waitFlag, cloneFlags, test_helper::CloneHelper::doNothing,
                                    test_helper::CloneHelper::sleep_1sec);
}

TEST_F(WaitpidFlagsTest, waitpidWithCloneFlagFailsWhenChildSendsSIGCHLD) {
  unsigned int waitFlag = __WCLONE;
  int cloneFlags = SIGCHLD;
  test_helper::waitForChildFails(waitFlag, cloneFlags, test_helper::CloneHelper::sleep_1sec,
                                 test_helper::CloneHelper::doNothing);
  test_helper::waitForChildFails(waitFlag, cloneFlags, test_helper::CloneHelper::doNothing,
                                 test_helper::CloneHelper::sleep_1sec);
}

TEST_F(WaitpidFlagsTest, waitpidWithWallFlagSucceedsWhenChildSendsSIGCHLD) {
  unsigned int waitFlag = 0;
  int cloneFlags = SIGCHLD;
  test_helper::waitForChildSucceeds(waitFlag, cloneFlags, test_helper::CloneHelper::sleep_1sec,
                                    test_helper::CloneHelper::doNothing);
  test_helper::waitForChildSucceeds(waitFlag, cloneFlags, test_helper::CloneHelper::doNothing,
                                    test_helper::CloneHelper::sleep_1sec);
}

TEST_F(WaitpidFlagsTest, waitpidFailsWaitingWhenChildIsNotThreadGroupLeader) {
  unsigned int waitFlag = __WALL;
  int cloneFlags = SIGCHLD | CLONE_VM | CLONE_THREAD | CLONE_SIGHAND;
  test_helper::waitForChildFails(waitFlag, cloneFlags, test_helper::CloneHelper::doNothing,
                                 test_helper::CloneHelper::sleep_1sec);
}
