// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_MISTOS_SYSLOG_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_MISTOS_SYSLOG_H_

#include <lib/mistos/starnix/kernel/vfs/file_object.h>

namespace starnix {

class SyslogFile : public FileOps {
 public:
  static FileHandle new_file(const CurrentTask& current_task);

  bool is_seekable() const final { return false; }

  fit::result<Errno, off_t> seek(const FileObject& file, const CurrentTask& current_task,
                                 off_t current_offset, SeekTarget target) final {
    return fit::error(errno(ESPIPE));
  }

  fit::result<Errno, size_t> read(/*Locked<FileOpsCore>& locked,*/ const FileObject& file,
                                  const CurrentTask& current_task, size_t offset,
                                  OutputBuffer* data) final {
    DEBUG_ASSERT(offset == 0);
    return fit::ok(0);
  }

  fit::result<Errno, size_t> write(/*Locked<WriteOps>& locked,*/ const FileObject& file,
                                   const CurrentTask& current_task, size_t offset,
                                   InputBuffer* data) final {
    DEBUG_ASSERT(offset == 0);
    return fit::error(errno(ENOTSUP));
  }
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_FS_MISTOS_SYSLOG_H_
