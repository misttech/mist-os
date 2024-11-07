// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/fdio/fdio_state.h"

__CONSTINIT fdio_state_t __fdio_global_state = {
    .cwd_path = fdio_internal::PathBuffer('/'),
};
