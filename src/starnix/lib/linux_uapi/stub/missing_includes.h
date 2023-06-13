// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_LIB_LINUX_UAPI_STUB_MISSING_INCLUDES_H_
#define SRC_STARNIX_LIB_LINUX_UAPI_STUB_MISSING_INCLUDES_H_

// Adding includes that are not detected by rust-bindings because they are
// defined using functions

#include <asm/ioctls.h>

const __u32 _TIOCSPTLCK = TIOCSPTLCK;
#undef TIOCSPTLCK
const __u32 TIOCSPTLCK = _TIOCSPTLCK;

const __u32 _TIOCGPTLCK = TIOCGPTLCK;
#undef TIOCGPTLCK
const __u32 TIOCGPTLCK = _TIOCGPTLCK;

const __u32 _TIOCGPKT = TIOCGPKT;
#undef TIOCGPKT
const __u32 TIOCGPKT = _TIOCGPKT;

const __u32 _TIOCSIG = TIOCSIG;
#undef TIOCSIG
const __u32 TIOCSIG = _TIOCSIG;

const __u32 _TIOCGPTN = TIOCGPTN;
#undef TIOCGPTN
const __u32 TIOCGPTN = _TIOCGPTN;

const __u32 _BINDER_WRITE_READ = BINDER_WRITE_READ;
#undef BINDER_WRITE_READ
const __u32 BINDER_WRITE_READ = _BINDER_WRITE_READ;

const __u32 _BINDER_SET_IDLE_TIMEOUT = BINDER_SET_IDLE_TIMEOUT;
#undef BINDER_SET_IDLE_TIMEOUT
const __u32 BINDER_SET_IDLE_TIMEOUT = _BINDER_SET_IDLE_TIMEOUT;

const __u32 _BINDER_SET_MAX_THREADS = BINDER_SET_MAX_THREADS;
#undef BINDER_SET_MAX_THREADS
const __u32 BINDER_SET_MAX_THREADS = _BINDER_SET_MAX_THREADS;

const __u32 _BINDER_SET_IDLE_PRIORITY = BINDER_SET_IDLE_PRIORITY;
#undef BINDER_SET_IDLE_PRIORITY
const __u32 BINDER_SET_IDLE_PRIORITY = _BINDER_SET_IDLE_PRIORITY;

const __u32 _BINDER_SET_CONTEXT_MGR = BINDER_SET_CONTEXT_MGR;
#undef BINDER_SET_CONTEXT_MGR
const __u32 BINDER_SET_CONTEXT_MGR = _BINDER_SET_CONTEXT_MGR;

const __u32 _BINDER_THREAD_EXIT = BINDER_THREAD_EXIT;
#undef BINDER_THREAD_EXIT
const __u32 BINDER_THREAD_EXIT = _BINDER_THREAD_EXIT;

const __u32 _BINDER_VERSION = BINDER_VERSION;
#undef BINDER_VERSION
const __u32 BINDER_VERSION = _BINDER_VERSION;

const __u32 _BINDER_GET_NODE_DEBUG_INFO = BINDER_GET_NODE_DEBUG_INFO;
#undef BINDER_GET_NODE_DEBUG_INFO
const __u32 BINDER_GET_NODE_DEBUG_INFO = _BINDER_GET_NODE_DEBUG_INFO;

const __u32 _BINDER_GET_NODE_INFO_FOR_REF = BINDER_GET_NODE_INFO_FOR_REF;
#undef BINDER_GET_NODE_INFO_FOR_REF
const __u32 BINDER_GET_NODE_INFO_FOR_REF = _BINDER_GET_NODE_INFO_FOR_REF;

const __u32 _BINDER_SET_CONTEXT_MGR_EXT = BINDER_SET_CONTEXT_MGR_EXT;
#undef BINDER_SET_CONTEXT_MGR_EXT
const __u32 BINDER_SET_CONTEXT_MGR_EXT = _BINDER_SET_CONTEXT_MGR_EXT;

const __u32 _BINDER_FREEZE = BINDER_FREEZE;
#undef BINDER_FREEZE
const __u32 BINDER_FREEZE = _BINDER_FREEZE;

const __u32 _BINDER_GET_FROZEN_INFO = BINDER_GET_FROZEN_INFO;
#undef BINDER_GET_FROZEN_INFO
const __u32 BINDER_GET_FROZEN_INFO = _BINDER_GET_FROZEN_INFO;

const __u32 _BINDER_ENABLE_ONEWAY_SPAM_DETECTION = BINDER_ENABLE_ONEWAY_SPAM_DETECTION;
#undef BINDER_ENABLE_ONEWAY_SPAM_DETECTION
const __u32 BINDER_ENABLE_ONEWAY_SPAM_DETECTION = _BINDER_ENABLE_ONEWAY_SPAM_DETECTION;

const __u32 _EVIOCGVERSION = EVIOCGVERSION;
#undef EVIOCGVERSION
const __u32 EVIOCGVERSION = _EVIOCGVERSION;

const __u32 _EVIOCGID = EVIOCGID;
#undef EVIOCGID
const __u32 EVIOCGID = _EVIOCGID;

// `EVIOCGBIT`, `EVIOCGPROP`, and `EVIOCGABS` are invoked with various paraemters to query
// metadata about an input device. Create Rust symbols for the commonly used invocations.
//
// The `EVIOCGBIT` invocations specify a `size` _just_ large enough to report all off the
// feature bits for that attribute.
//
// TODO(quiche): Eventually, it will probably be better to provide a way to parse the fields
// within an `ioctl()`'s `request` parameter. That would allow, e.g., the input code to
// respond to any request for `EV_KEY` feature bits, even if the caller provided a buffer
// larger than that needed for the available bits.
#define N_BYTES(BITS) (((BITS) + CHAR_BIT - 1) / CHAR_BIT)
const __u32 EVIOCGBIT_EV_KEY = EVIOCGBIT(EV_KEY, N_BYTES(KEY_MAX));
const __u32 EVIOCGBIT_EV_ABS = EVIOCGBIT(EV_ABS, N_BYTES(ABS_MAX));
const __u32 EVIOCGBIT_EV_REL = EVIOCGBIT(EV_REL, N_BYTES(REL_MAX));
const __u32 EVIOCGBIT_EV_SW = EVIOCGBIT(EV_SW, N_BYTES(SW_MAX));
const __u32 EVIOCGBIT_EV_LED = EVIOCGBIT(EV_LED, N_BYTES(LED_MAX));
const __u32 EVIOCGBIT_EV_FF = EVIOCGBIT(EV_FF, N_BYTES(FF_MAX));
const __u32 EVIOCGBIT_EV_MSC = EVIOCGBIT(EV_MSC, N_BYTES(MSC_MAX));
const __u32 EVIOCGPROP = EVIOCGPROP(N_BYTES(INPUT_PROP_MAX));
const __u32 EVIOCGABS_X = EVIOCGABS(ABS_X);
const __u32 EVIOCGABS_Y = EVIOCGABS(ABS_Y);
#undef N_BYTES

// Symbols for remote binder device driver

struct remote_binder_start_command {
  const char* incoming_service;
};

struct remote_binder_wait_command {
  char spawn_thread;
};

const __u32 _REMOTE_BINDER_START = _IOR('R', 1, struct remote_binder_start_command);
const __u32 REMOTE_BINDER_START = _REMOTE_BINDER_START;

const __u32 _REMOTE_BINDER_WAIT = _IOW('R', 2, struct remote_binder_wait_command);
const __u32 REMOTE_BINDER_WAIT = _REMOTE_BINDER_WAIT;

#endif  // SRC_STARNIX_LIB_LINUX_UAPI_STUB_MISSING_INCLUDES_H_
