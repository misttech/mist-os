// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_INCLUDE_KERNEL_PREEMPT_DISABLED_TOKEN_H_
#define ZIRCON_KERNEL_INCLUDE_KERNEL_PREEMPT_DISABLED_TOKEN_H_

#include <lib/zircon-internal/thread_annotations.h>

class PreemptionState;

// The PreemptDisabledToken (and its global singleton instance,
// |preempt_disabled_token|) are clang static analysis tokens which can be used
// to annotate methods as requiring that local preemption be disabled in order
// to operate properly.  See the AnnotatedAutoPreemptDisabler helper in
// kernel/auto_preempt_disabler.h for more details.
struct TA_CAP("token") PreemptDisabledToken {
 public:
  void AssertHeld() TA_ASSERT();

  PreemptDisabledToken() = default;
  PreemptDisabledToken(const PreemptDisabledToken&) = delete;
  PreemptDisabledToken(PreemptDisabledToken&&) = delete;
  PreemptDisabledToken& operator=(const PreemptDisabledToken&) = delete;
  PreemptDisabledToken& operator=(PreemptDisabledToken&&) = delete;

 private:
  friend class PreemptionState;
  void Acquire() TA_ACQ() {}
  void Release() TA_REL() {}
};

extern PreemptDisabledToken preempt_disabled_token;

#endif  // ZIRCON_KERNEL_INCLUDE_KERNEL_PREEMPT_DISABLED_TOKEN_H_
