// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tls-dep.h"

#include <zircon/compiler.h>

__EXPORT thread_local int tls_dep_data = kTlsDepDataValue;
__EXPORT alignas(kTlsDepAlign) thread_local char tls_dep_bss[2];
