// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Configuration macros for the pw_bluetooth_sapphire module.
#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_CONFIG_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_CONFIG_H_

// Disable Fuchsia inspect and tracing code by default.
#ifndef PW_BLUETOOTH_SAPPHIRE_INSPECT_ENABLED
#define NINSPECT 1
#endif  // PW_BLUETOOTH_SAPPHIRE_INSPECT_ENABLED
#ifndef PW_BLUETOOTH_SAPPHIRE_TRACE_ENABLED
#define NTRACE 1
#endif  // PW_BLUETOOTH_SAPPHIRE_TRACE_ENABLED

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_CONFIG_H_
