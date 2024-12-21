// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/directory_file.h"

#include <lib/mistos/starnix/kernel/vfs/file_object.h>

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

MemoryDirectoryFile* MemoryDirectoryFile::New() {
  fbl::AllocChecker ac;
  auto ptr = new (&ac) MemoryDirectoryFile();
  ZX_ASSERT(ac.check());
  return ptr;
}

fit::result<Errno, off_t> MemoryDirectoryFile::seek(const FileObject& file,
                                                    const CurrentTask& current_task,
                                                    off_t current_offset, SeekTarget target) const {
  auto new_offset = default_seek(current_offset, target, [](off_t) -> fit::result<Errno, off_t> {
    return fit::error(errno(EINVAL));
  }) _EP(new_offset);
  // Nothing to do.
  if (current_offset == new_offset.value()) {
    return fit::ok(new_offset.value());
  }

  auto readdir_position = readdir_position_.Lock();

  // We use 0 and 1 for "." and ".."
  if (new_offset.value() <= 2) {
    *readdir_position = util::Bound<FsString>::Unbounded();
  } else {
    file.name_->entry_->get_children<void>([&](auto& children) -> void {
      size_t count = static_cast<size_t>(new_offset.value() - 2);
      auto iter = children.begin();
      for (size_t i = 0; i < count && iter != children.end(); i++, ++iter) {
        if (i == count - 1) {
          *readdir_position = util::Bound<FsString>::Excluded(iter->first);
          break;
        }
      }
      if (iter == children.end()) {
        *readdir_position = util::Bound<FsString>::Unbounded();
      }
    });
  }

  return fit::ok(new_offset.value());
}

fit::result<Errno> MemoryDirectoryFile::readdir(const FileObject& file,
                                                const CurrentTask& current_task,
                                                DirentSink* sink) const {
  _EP(emit_dotdot(file, sink));

  auto readdir_position = readdir_position_.Lock();

  return file.name_->entry_->get_children<fit::result<Errno>>(
      [&](auto& children) -> fit::result<Errno> {
        auto [begin, end] = children.range(*readdir_position, util::Bound<FsString>::Unbounded());
        for (auto iter = begin; iter != end; ++iter) {
          if (auto entry = iter->second.Lock()) {
            auto mode = entry->node_->info()->mode_;
            _EP(sink->add(entry->node_->node_id_, sink->offset() + 1,
                          DirectoryEntryType::from_mode(mode), iter->first));
            *readdir_position = util::Bound<FsString>::Excluded(iter->first);
          }
        }
        return fit::ok();
      });
}

MemoryDirectoryFile::MemoryDirectoryFile() = default;

}  // namespace starnix
