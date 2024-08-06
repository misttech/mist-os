// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_MACROS_H_
#define SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_MACROS_H_

#include <lib/driver/logging/cpp/logger.h>

#define LOG(severity, fmt, ...) FDF_LOG(severity, fmt, ##__VA_ARGS__)

#endif  // SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_MACROS_H_
