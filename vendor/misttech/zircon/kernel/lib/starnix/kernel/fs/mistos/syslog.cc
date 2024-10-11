// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/fs/mistos/syslog.h"

#include <lib/mistos/starnix/kernel/vfs/anon_node.h>
#include <lib/mistos/starnix/kernel/vfs/buffers/io_buffers.h>
#include <lib/mistos/starnix/kernel/vfs/module.h>

#include <fbl/alloc_checker.h>
#include <ktl/unique_ptr.h>

namespace starnix {

FileHandle SyslogFile::new_file(const CurrentTask& current_task) {
  fbl::AllocChecker ac;
  auto file = ktl::make_unique<SyslogFile>(&ac);
  ASSERT(ac.check());
  return Anon::new_file(current_task, ktl::move(file), OpenFlags(OpenFlagsEnum::RDWR));
}

fit::result<Errno, size_t> SyslogFile::write(const FileObject& file,
                                             const CurrentTask& current_task, size_t offset,
                                             InputBuffer* data) {
  DEBUG_ASSERT(offset == 0);
  return data->read_each([](const ktl::span<uint8_t>& bytes) {
    printf("%.*s", static_cast<int>(bytes.size()), bytes.data());
    return fit::ok(bytes.size());
  });
}

}  // namespace starnix
