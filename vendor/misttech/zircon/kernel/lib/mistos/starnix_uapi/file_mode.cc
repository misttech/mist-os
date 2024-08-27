// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix_uapi/file_mode.h"

namespace starnix_uapi {

const FileMode FileMode::IFLNK = FileMode(S_IFLNK);
const FileMode FileMode::IFREG = FileMode(S_IFREG);
const FileMode FileMode::IFDIR = FileMode(S_IFDIR);
const FileMode FileMode::IFCHR = FileMode(S_IFCHR);
const FileMode FileMode::IFBLK = FileMode(S_IFBLK);
const FileMode FileMode::IFIFO = FileMode(S_IFIFO);
const FileMode FileMode::IFSOCK = FileMode(S_IFSOCK);

const FileMode FileMode::IFMT = FileMode(S_IFMT);

const FileMode FileMode::DEFAULT_UMASK = FileMode(022);  // 0o022 in octal
const FileMode FileMode::ALLOW_ALL = FileMode(0777);     // 0o777 in octal
const FileMode FileMode::PERMISSIONS = FileMode(07777);  // 0o7777 in octal
const FileMode FileMode::EMPTY = FileMode(0);

}  // namespace starnix_uapi
