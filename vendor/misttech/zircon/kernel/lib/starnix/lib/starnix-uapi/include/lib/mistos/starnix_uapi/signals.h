// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_SIGNALS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_SIGNALS_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <zircon/types.h>

#include <asm/signal.h>

namespace starnix_uapi {

struct UncheckedSignal;

/// The `Signal` struct represents a valid signal.
struct Signal {
 private:
  uint32_t number_;

 public:
  // impl Signal

  /// The signal number, guaranteed to be a value between 1..=NUM_SIGNALS.
  uint32_t number() const { return number_; }

  /// Returns the bitmask for this signal number.
  uint64_t mask() const { return 1u << (number_ - 1); }

  /// Returns true if the signal is a real-time signal.
  bool is_real_time() const { return number_ >= SIGRTMIN; }

  /// Returns true if this signal can't be blocked. This means either SIGKILL or SIGSTOP.
  bool is_unblockable() const;

  /// Used exclusively for PTRACE_O_TRACESYSGOOD
  void set_ptrace_syscall_bit() { number_ |= 0x80; }

  static const uint32_t NUM_SIGNALS = 64;

 public:
  // impl TryFrom<UncheckedSignal>
  static fit::result<Errno, Signal> try_from(UncheckedSignal value);

 public:
  explicit Signal(uint32_t number) : number_(number) {}
};

static const Signal kSIGHUP = Signal(SIGHUP);
static const Signal kSIGINT = Signal(SIGINT);
static const Signal kSIGQUIT = Signal(SIGQUIT);
static const Signal kSIGILL = Signal(SIGILL);
static const Signal kSIGTRAP = Signal(SIGTRAP);
static const Signal kSIGABRT = Signal(SIGABRT);
static const Signal kSIGIOT = Signal(SIGIOT);
static const Signal kSIGBUS = Signal(SIGBUS);
static const Signal kSIGFPE = Signal(SIGFPE);
static const Signal kSIGKILL = Signal(SIGKILL);
static const Signal kSIGUSR1 = Signal(SIGUSR1);
static const Signal kSIGSEGV = Signal(SIGSEGV);
static const Signal kSIGUSR2 = Signal(SIGUSR2);
static const Signal kSIGPIPE = Signal(SIGPIPE);
static const Signal kSIGALRM = Signal(SIGALRM);
static const Signal kSIGTERM = Signal(SIGTERM);
static const Signal kSIGSTKFLT = Signal(SIGSTKFLT);
static const Signal kSIGCHLD = Signal(SIGCHLD);
static const Signal kSIGCONT = Signal(SIGCONT);
static const Signal kSIGSTOP = Signal(SIGSTOP);
static const Signal kSIGTSTP = Signal(SIGTSTP);
static const Signal kSIGTTIN = Signal(SIGTTIN);
static const Signal kSIGTTOU = Signal(SIGTTOU);
static const Signal kSIGURG = Signal(SIGURG);
static const Signal kSIGXCPU = Signal(SIGXCPU);
static const Signal kSIGXFSZ = Signal(SIGXFSZ);
static const Signal kSIGVTALRM = Signal(SIGVTALRM);
static const Signal kSIGPROF = Signal(SIGPROF);
static const Signal kSIGWINCH = Signal(SIGWINCH);
static const Signal kSIGIO = Signal(SIGIO);
static const Signal kSIGPWR = Signal(SIGPWR);
static const Signal kSIGSYS = Signal(SIGSYS);
static const Signal kSIGRTMIN = Signal(SIGRTMIN);

/// An unchecked signal represents a signal that has not been through verification, and may
/// represent an invalid signal number.
struct UncheckedSignal {
 public:
  static UncheckedSignal New(uint64_t value) { return UncheckedSignal(value); }
  static UncheckedSignal From(const Signal& value) {
    return UncheckedSignal(static_cast<uint64_t>(value.number()));
  }
  static UncheckedSignal From(uint32_t value) {
    return UncheckedSignal(static_cast<uint64_t>(value));
  }

  bool is_zero() { return value_ == 0; }

 public:
  // C++
  uint64_t value() { return value_; }

 private:
  explicit UncheckedSignal(uint64_t value) : value_(value) {}

  uint64_t value_;
};

struct SigSet {
 public:
  bool has_signal(const Signal& signal) const { return (value_ & signal.mask()) != 0; }

  static SigSet From(sigset_t value) { return SigSet(value); }
  static SigSet From(Signal value) { return SigSet(value.mask()); }

 private:
  explicit SigSet(unsigned long value) : value_(value) {}

  unsigned long value_;
};

static_assert(sizeof(SigSet) == sizeof(sigset_t));

static const SigSet UNBLOCKABLE_SIGNALS = SigSet::From(kSIGKILL.mask() | kSIGSTOP.mask());

}  // namespace starnix_uapi

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_UAPI_INCLUDE_LIB_MISTOS_STARNIX_UAPI_SIGNALS_H_
