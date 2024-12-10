// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/directory_file.h"

namespace starnix {

fit::result<Errno> emit_dotdot(const FileObject& file, DirentSink* sink) {
  if (sink->offset() == 0) {
    _EP(sink->add(file.node()->node_id_, 1, DirectoryEntryType::DIR, "."));
  }
  if (sink->offset() == 1) {
    _EP(sink->add(file.name_->entry_->parent_or_self()->node_->node_id_, 2, DirectoryEntryType::DIR,
                  ".."));
  }
  return fit::ok();
}

}  // namespace starnix
