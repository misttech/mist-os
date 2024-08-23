// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_LEGACY_BIND_CONSTANTS_LEGACY_BIND_CONSTANTS_H_
#define LIB_DRIVER_LEGACY_BIND_CONSTANTS_LEGACY_BIND_CONSTANTS_H_

// LINT.IfChange
// global binding variables at 0x00XX
#define BIND_PROTOCOL 0x0001  // primary protocol of the device

#define BIND_PCI_TOPO_PACK(bus, dev, func) (((bus) << 8) | (dev << 3) | (func))

// Platform bus binding variables at 0x03XX
#define BIND_PLATFORM_DEV_VID 0x0300
#define BIND_PLATFORM_DEV_PID 0x0301
#define BIND_PLATFORM_DEV_DID 0x0302
#define BIND_PLATFORM_DEV_INSTANCE_ID 0x0304

// LINT.ThenChange(/src/lib/ddk/include/lib/ddk/binding_priv.h)

#endif  // LIB_DRIVER_LEGACY_BIND_CONSTANTS_LEGACY_BIND_CONSTANTS_H_
