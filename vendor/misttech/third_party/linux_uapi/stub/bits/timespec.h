// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_LINUX_UAPI_STUB_BITS_TIMESPEC_H_
#define ZIRCON_KERNEL_LIB_MISTOS_LINUX_UAPI_STUB_BITS_TIMESPEC_H_

struct timespec {
  __kernel_time_t tv_sec;
  long tv_nsec;
};

#endif  // ZIRCON_KERNEL_LIB_MISTOS_LINUX_UAPI_STUB_BITS_TIMESPEC_H_
