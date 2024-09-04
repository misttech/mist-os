// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/xattr.h"

#include <linux/errno.h>

namespace starnix {

fit::result<Errno, FsString> MemoryXattrStorage::get_xattr(const FsStr& name) const {
  auto _xattrs = xattrs.Lock();
  auto value = _xattrs->find(name);
  if (value != _xattrs->end()) {
    return fit::ok((*value).value);
  } else {
    return fit::error(errno(ENODATA));
  }
}

fit::result<Errno> MemoryXattrStorage::set_xattr(const FsStr& name, const FsStr& value,
                                                 XattrOp op) const {
  return fit::error(errno(ENOTSUP));
}

fit::result<Errno> MemoryXattrStorage::remove_xattr(const FsStr& name) const {
  return fit::error(errno(ENOTSUP));
}

MemoryXattrStorage MemoryXattrStorage::Default() { return {}; }

}  // namespace starnix
