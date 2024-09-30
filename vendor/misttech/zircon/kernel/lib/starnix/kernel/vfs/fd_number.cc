// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/fd_number.h"

#include <linux/fcntl.h>

namespace starnix {

const FdNumber FdNumber::AT_FDCWD_(AT_FDCWD);

}

// #define AT_FDCWD AT_FDCWD_TMP
