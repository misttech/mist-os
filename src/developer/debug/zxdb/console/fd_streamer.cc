// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/zxdb/console/fd_streamer.h"

#include <memory>
#include <utility>

#include "src/developer/debug/zxdb/console/console.h"

namespace zxdb {

std::unique_ptr<debug::BufferedFD> StreamFDToConsole(fbl::unique_fd fd) {
  auto streamer = std::make_unique<debug::BufferedFD>(std::move(fd));
  streamer->set_data_available_callback([streamer = streamer.get()]() {
    Console* console = Console::get();
    if (!console)
      return;

    OutputBuffer data;
    constexpr size_t kReadSize = 4024;  // Read in 4K chunks for no particular reason.
    auto& stream = streamer->stream();
    while (true) {
      char buf[kReadSize];

      size_t read_amount = stream.Read(buf, kReadSize);
      data.Append(std::string(buf, read_amount));

      if (read_amount < kReadSize)
        break;
    }
    if (!data.empty()) {
      console->Write(data);
    }
  });
  streamer->Start();
  return streamer;
}

}  // namespace zxdb
