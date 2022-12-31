// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <getopt.h>
#include <lib/stdcompat/span.h>
#include <unistd.h>
#include <zircon/assert.h>

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <locale>
#include <string_view>
#include <thread>
#include <vector>

#include <fbl/unique_fd.h>

#ifdef __Fuchsia__
#include <zircon/syscalls.h>
#endif

namespace {

constexpr std::string_view kStdinoutFilename = "-";

constexpr char kOptString[] = "c:e:m:M:o:t:w:x:";
constexpr option kLongOpts[] = {
    {"cat-from", required_argument, nullptr, 'c'},      //
    {"cat-to", required_argument, nullptr, 'o'},        //
    {"echo", required_argument, nullptr, 'e'},          //
    {"memory", required_argument, nullptr, 'm'},        //
    {"memory-ints", required_argument, nullptr, 'M'},   //
    {"threads", required_argument, nullptr, 't'},       //
    {"memory-wchar", required_argument, nullptr, 'w'},  //
    {"exit", required_argument, nullptr, 'x'},          //
};

int Usage() {
  fprintf(stderr,
          "Usage: test-child [--echo=STRING] [--memory=STRING] [--memory-ints=INT,...] "
          "[--cat-from=FILE] [--cat-to=FILE] "
          "[--threads=N]\n");
  return 1;
}

[[noreturn]] void Hang() {
  while (true) {
#ifdef __Fuchsia__
    zx_thread_legacy_yield(0);
#else
    pause();
#endif
  }
}

void Cat(fbl::unique_fd from, int to) {
  char buf[BUFSIZ];
  ssize_t nread;
  while ((nread = read(from.get(), buf, sizeof(buf))) > 0) {
    cpp20::span<const char> chunk(buf, static_cast<size_t>(nread));
    while (!chunk.empty()) {
      ssize_t nwrote = write(to, chunk.data(), chunk.size());
      if (nwrote < 0) {
        perror("write");
        exit(2);
      }
      chunk = chunk.subspan(static_cast<size_t>(nwrote));
    }
  }
  if (nread < 0) {
    perror("read");
    exit(2);
  }
}

fbl::unique_fd CatOpen(const char* filename, int stdfd, int oflags) {
  fbl::unique_fd fd{filename == kStdinoutFilename ? stdfd : open(filename, oflags, 0666)};
  if (!fd) {
    perror(filename);
    exit(2);
  }
  return fd;
}

void CatFrom(const char* filename) {
  Cat(CatOpen(filename, STDIN_FILENO, O_RDONLY), STDOUT_FILENO);
}

void CatTo(const char* filename) {
  Cat(fbl::unique_fd{STDIN_FILENO},
      CatOpen(filename, STDOUT_FILENO, O_WRONLY | O_CREAT | O_EXCL).get());
}

}  // namespace

int main(int argc, char** argv) {
  size_t thread_count = 0;
  std::vector<int> ints;
  std::wstring wstr;

  while (true) {
    switch (getopt_long(argc, argv, kOptString, kLongOpts, nullptr)) {
      case -1:
        // This ends the loop.  All other cases continue (or return).
        break;

      case 'c':
        CatFrom(optarg);
        continue;

      case 'o':
        CatTo(optarg);
        continue;

      case 'e':
        puts(optarg);
        continue;

      case 'm':
        printf("%p\n", optarg);
        continue;

      case 'M': {
        std::string optstring = optarg;
        char* rest = optstring.data();
        char* p;
        while ((p = strsep(&rest, ",")) != nullptr) {
          ints.push_back(atoi(p));
        }
        printf("%p\n", ints.data());
        continue;
      }

      case 't':
        thread_count = atoi(optarg);
        continue;

      case 'w': {
        std::string_view byte_string = optarg;
        wstr.resize(byte_string.size());
        wstr.resize(mbstowcs(wstr.data(), byte_string.data(), wstr.size()));
        ZX_ASSERT(wstr.size() == byte_string.size());
        printf("%p\n", wstr.data());
        continue;
      }

      case 'x':
        return atoi(optarg);

      default:
        return Usage();
    }
    break;
  }
  if (optind != argc) {
    return Usage();
  }

  std::vector<std::thread> threads(thread_count);
  for (std::thread& thread : threads) {
    thread = std::thread(Hang);
  }
  if (thread_count > 0) {
    printf("started %zu additional threads\n", thread_count);
  }

  fflush(stdout);

  Hang();

  return 0;
}
