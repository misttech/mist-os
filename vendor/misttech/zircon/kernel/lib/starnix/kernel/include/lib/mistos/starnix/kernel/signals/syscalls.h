// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SIGNALS_SYSCALLS_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SIGNALS_SYSCALLS_H_

#include <lib/fit/result.h>
#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/user_address.h>

#include <linux/resource.h>
#include <linux/wait.h>

namespace starnix {

class CurrentTask;
class ProcessSelector;
struct WaitResult;

class WaitingOptions {
 public:
  WaitingOptions(uint32_t options)
      : wait_for_exited_(options & WEXITED),
        wait_for_stopped_(options & WSTOPPED),
        wait_for_continued_(options & WCONTINUED),
        block_((options & WNOHANG) == 0),
        keep_waitable_state_(options & WNOWAIT),
        wait_for_all_(options & __WALL),
        wait_for_clone_(options & __WCLONE),
        waiter_(ktl::nullopt) {}

  /// Build a `WaitingOptions` from the waiting flags of waitid.
  static fit::result<Errno, WaitingOptions> new_for_waitid(uint32_t options) {
    if (options & ~(__WCLONE | __WALL | WNOHANG | WNOWAIT | WSTOPPED | WEXITED | WCONTINUED)) {
      return fit::error(errno(EINVAL));
    }
    if ((options & (WEXITED | WSTOPPED | WCONTINUED)) == 0) {
      return fit::error(errno(EINVAL));
    }
    return fit::ok(WaitingOptions(options));
  }

  /// Build a `WaitingOptions` from the waiting flags of wait4.
  static fit::result<Errno, WaitingOptions> new_for_wait4(uint32_t options, pid_t waiter_pid) {
    if (options & ~(__WCLONE | __WALL | WNOHANG | WUNTRACED | WCONTINUED)) {
      return fit::error(errno(EINVAL));
    }
    WaitingOptions result(options | WEXITED);
    result.waiter_ = waiter_pid;
    return fit::ok(result);
  }

  bool wait_for_exited() const { return wait_for_exited_; }
  bool wait_for_stopped() const { return wait_for_stopped_; }
  bool wait_for_continued() const { return wait_for_continued_; }
  bool block() const { return block_; }
  bool keep_waitable_state() const { return keep_waitable_state_; }
  bool wait_for_all() const { return wait_for_all_; }
  bool wait_for_clone() const { return wait_for_clone_; }

 private:
  bool wait_for_exited_;
  bool wait_for_stopped_;
  bool wait_for_continued_;
  bool block_;
  bool keep_waitable_state_;
  bool wait_for_all_;
  bool wait_for_clone_;
  ktl::optional<pid_t> waiter_;
};

fit::result<Errno, pid_t> sys_wait4(const CurrentTask& current_task, pid_t raw_selector,
                                    starnix_uapi::UserRef<int32_t> user_wstatus, uint32_t options,
                                    starnix_uapi::UserRef<struct ::rusage> user_rusage);

fit::result<Errno, ktl::optional<WaitResult>> friend_wait_on_pid(const CurrentTask& current_task,
                                                                 const ProcessSelector& selector,
                                                                 const WaitingOptions& options);

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_SIGNALS_SYSCALLS_H_
