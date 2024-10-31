// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/dirent_sink.h"

namespace starnix {

const DirectoryEntryType DirectoryEntryType::UNKNOWN = DirectoryEntryType(0);
const DirectoryEntryType DirectoryEntryType::FIFO = DirectoryEntryType(1);
const DirectoryEntryType DirectoryEntryType::CHR = DirectoryEntryType(2);
const DirectoryEntryType DirectoryEntryType::DIR = DirectoryEntryType(4);
const DirectoryEntryType DirectoryEntryType::BLK = DirectoryEntryType(6);
const DirectoryEntryType DirectoryEntryType::REG = DirectoryEntryType(8);
const DirectoryEntryType DirectoryEntryType::LNK = DirectoryEntryType(10);
const DirectoryEntryType DirectoryEntryType::SOCK = DirectoryEntryType(12);

}  // namespace starnix
