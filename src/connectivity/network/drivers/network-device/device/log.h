// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_LOG_H_
#define SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_LOG_H_

#ifdef NETDEV_DRIVER
#include <sdk/lib/driver/logging/cpp/logger.h>
#define LOG_ERROR(msg) FDF_LOG(ERROR, msg)
#define LOG_WARN(msg) FDF_LOG(WARNING, msg)
#define LOG_INFO(msg) FDF_LOG(INFO, msg)
#define LOG_TRACE(msg) FDF_LOG(DEBUG, msg)

#define LOGF_ERROR(fmt, ...) FDF_LOG(ERROR, fmt, ##__VA_ARGS__)
#define LOGF_WARN(fmt, ...) FDF_LOG(WARNING, fmt, ##__VA_ARGS__)
#define LOGF_INFO(fmt, ...) FDF_LOG(INFO, fmt, ##__VA_ARGS__)
#define LOGF_TRACE(fmt, ...) FDF_LOG(DEBUG, fmt, ##__VA_ARGS__)

#else
#include <lib/syslog/global.h>
#define LOG_ERROR(msg) FX_LOG(ERROR, "network-device", msg)
#define LOG_WARN(msg) FX_LOG(WARNING, "network-device", msg)
#define LOG_INFO(msg) FX_LOG(INFO, "network-device", msg)
#define LOG_TRACE(msg) FX_LOG(DEBUG, "network-device", msg)

#define LOGF_ERROR(fmt, ...) FX_LOGF(ERROR, "network-device", fmt, ##__VA_ARGS__)
#define LOGF_WARN(fmt, ...) FX_LOGF(WARNING, "network-device", fmt, ##__VA_ARGS__)
#define LOGF_INFO(fmt, ...) FX_LOGF(INFO, "network-device", fmt, ##__VA_ARGS__)
#define LOGF_TRACE(fmt, ...) FX_LOGF(DEBUG, "network-device", fmt, ##__VA_ARGS__)

#endif

#endif  // SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_DEVICE_LOG_H_
