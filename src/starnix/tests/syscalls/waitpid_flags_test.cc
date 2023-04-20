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
SignalMaskHelper sigmaskHelper = SignalMaskHelper();
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
  waitForChildFails(waitFlag, cloneFlags, CloneHelper::sleep_1sec, CloneHelper::doNothing);
  waitForChildFails(waitFlag, cloneFlags, CloneHelper::doNothing, CloneHelper::sleep_1sec);
}

TEST_F(WaitpidFlagsTest, waitpidWithCloneFlagSucceedsWhenChildSendsNoSignal) {
  unsigned int waitFlag = __WCLONE;
  int cloneFlags = 0;
  waitForChildSucceeds(waitFlag, cloneFlags, CloneHelper::sleep_1sec, CloneHelper::doNothing);
  waitForChildSucceeds(waitFlag, cloneFlags, CloneHelper::doNothing, CloneHelper::sleep_1sec);
}

TEST_F(WaitpidFlagsTest, waitpidWithWallFlagSucceedsWhenChildSendsNoSignal) {
  unsigned int waitFlag = __WALL;
  int cloneFlags = 0;
  waitForChildSucceeds(waitFlag, cloneFlags, CloneHelper::sleep_1sec, CloneHelper::doNothing);
  waitForChildSucceeds(waitFlag, cloneFlags, CloneHelper::doNothing, CloneHelper::sleep_1sec);
}

TEST_F(WaitpidFlagsTest, waitpidWithNoFlagsFailsWhenChildSendsSIGUSR1) {
  unsigned int waitFlag = 0;
  int cloneFlags = SIGUSR1;
  waitForChildFails(waitFlag, cloneFlags, CloneHelper::sleep_1sec, CloneHelper::doNothing);
  sigmaskHelper.waitForSignal(SIGUSR1);
  waitForChildFails(waitFlag, cloneFlags, CloneHelper::doNothing, CloneHelper::sleep_1sec);
  sigmaskHelper.waitForSignal(SIGUSR1);
}

TEST_F(WaitpidFlagsTest, waipidWithCloneFlagSucceedsWhenChildSendsSIGUSR1) {
  unsigned int waitFlag = __WCLONE;
  int cloneFlags = SIGUSR1;
  waitForChildSucceeds(waitFlag, cloneFlags, CloneHelper::sleep_1sec, CloneHelper::doNothing);
  sigmaskHelper.waitForSignal(SIGUSR1);
  waitForChildSucceeds(waitFlag, cloneFlags, CloneHelper::doNothing, CloneHelper::sleep_1sec);
  sigmaskHelper.waitForSignal(SIGUSR1);
}

TEST_F(WaitpidFlagsTest, waipidWithWallFlagSucceedsWhenChildSendsSIGUSR1) {
  unsigned int waitFlag = __WALL;
  int cloneFlags = SIGUSR1;
  waitForChildSucceeds(waitFlag, cloneFlags, CloneHelper::sleep_1sec, CloneHelper::doNothing);
  sigmaskHelper.waitForSignal(SIGUSR1);
  waitForChildSucceeds(waitFlag, cloneFlags, CloneHelper::doNothing, CloneHelper::sleep_1sec);
  sigmaskHelper.waitForSignal(SIGUSR1);
}

TEST_F(WaitpidFlagsTest, waitpidWithNoFlagsSucceedsWhenChildSendsSIGCHLD) {
  unsigned int waitFlag = 0;
  int cloneFlags = SIGCHLD;
  waitForChildSucceeds(waitFlag, cloneFlags, CloneHelper::sleep_1sec, CloneHelper::doNothing);
  waitForChildSucceeds(waitFlag, cloneFlags, CloneHelper::doNothing, CloneHelper::sleep_1sec);
}

TEST_F(WaitpidFlagsTest, waitpidWithCloneFlagFailsWhenChildSendsSIGCHLD) {
  unsigned int waitFlag = __WCLONE;
  int cloneFlags = SIGCHLD;
  waitForChildFails(waitFlag, cloneFlags, CloneHelper::sleep_1sec, CloneHelper::doNothing);
  waitForChildFails(waitFlag, cloneFlags, CloneHelper::doNothing, CloneHelper::sleep_1sec);
}

TEST_F(WaitpidFlagsTest, waitpidWithWallFlagSucceedsWhenChildSendsSIGCHLD) {
  unsigned int waitFlag = 0;
  int cloneFlags = SIGCHLD;
  waitForChildSucceeds(waitFlag, cloneFlags, CloneHelper::sleep_1sec, CloneHelper::doNothing);
  waitForChildSucceeds(waitFlag, cloneFlags, CloneHelper::doNothing, CloneHelper::sleep_1sec);
}

TEST_F(WaitpidFlagsTest, waitpidFailsWaitingWhenChildIsNotThreadGroupLeader) {
  unsigned int waitFlag = __WALL;
  int cloneFlags = SIGCHLD | CLONE_VM | CLONE_THREAD | CLONE_SIGHAND;
  waitForChildFails(waitFlag, cloneFlags, CloneHelper::doNothing, CloneHelper::sleep_1sec);
}
