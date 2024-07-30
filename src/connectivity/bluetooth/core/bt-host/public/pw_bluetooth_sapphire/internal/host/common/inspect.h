// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_COMMON_INSPECT_H_
#define SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_COMMON_INSPECT_H_

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/config.h"

#ifdef NINSPECT
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/fake_inspect.h"
#else
#include "pw_preprocessor/compiler.h"
PW_MODIFY_DIAGNOSTICS_PUSH();
PW_MODIFY_DIAGNOSTIC(ignored, "-Wswitch-enum");
#include <lib/inspect/cpp/inspect.h>
PW_MODIFY_DIAGNOSTICS_POP();
#endif  // NINSPECT

#endif  // SRC_CONNECTIVITY_BLUETOOTH_CORE_BT_HOST_PUBLIC_PW_BLUETOOTH_SAPPHIRE_INTERNAL_HOST_COMMON_INSPECT_H_
