// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <fidl/fuchsia.hardware.skipblock/cpp/wire.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <lib/stdcompat/string_view.h>
#include <limits.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include <zircon/process.h>
#include <zircon/syscalls.h>

#include <memory>

#include <fbl/unique_fd.h>

int Usage() {
  fprintf(stderr, "usage: dd [OPTIONS]\n");
  fprintf(stderr, "dd can be used to convert and copy files\n");
  fprintf(stderr, " bs=BYTES  : read and write BYTES bytes at a time\n");
  fprintf(stderr, " count=N   : copy only N input blocks\n");
  fprintf(stderr, " ibs=BYTES : read BYTES bytes at a time (default: 512)\n");
  fprintf(stderr, " if=FILE   : read from FILE instead of stdin\n");
  fprintf(stderr, " obs=BYTES : write BYTES bytes at a time (default: 512)\n");
  fprintf(stderr, " of=FILE   : write to FILE instead of stdout\n");
  fprintf(stderr, " seek=N    : skip N obs-sized blocks at start of output\n");
  fprintf(stderr, " skip=N    : skip N ibs-sized blocks at start of input\n");
  fprintf(stderr, " conv=sync : pad input to input block size\n");
  fprintf(stderr,
          " N and BYTES may be followed by the following multiplicitive\n"
          " suffixes: c = 1, w = 2, b = 512, kB = 1000, K = 1024,\n"
          "           MB = 1000 * 1000, M = 1024 * 1024, xM = M,\n"
          "           GB = 1000 * 1000 * 1000, G = 1024 * 1024 * 1024\n");
  fprintf(stderr, " --help : Show this help message\n");
  return -1;
}

// Returns "true" if the argument matches the prefix.
// In this case, moves the argument past the prefix.
bool PrefixMatch(const char** arg, std::string_view prefix) {
  if (cpp20::starts_with(std::string_view(*arg), prefix)) {
    *arg += prefix.length();
    return true;
  }
  return false;
}

#define MAYBE_MULTIPLY_SUFFIX(str, out, suffix, value) \
  if (std::string_view(str) == (suffix)) {             \
    (out) *= (value);                                  \
    return 0;                                          \
  }

// Parse the formatted size string from |s|, and place
// the result in |out|.
//
// Returns 0 on success.
int ParseSize(const char* s, size_t* out) {
  char* endptr;
  if (*s < '0' || *s > '9') {
    goto done;
  }
  *out = strtol(s, &endptr, 10);
  if (*endptr == '\0') {
    return 0;
  }

  MAYBE_MULTIPLY_SUFFIX(endptr, *out, "G", UINT64_C(1) << 30);
  MAYBE_MULTIPLY_SUFFIX(endptr, *out, "GB", UINT64_C(1000) * UINT64_C(1000) * UINT64_C(1000));
  MAYBE_MULTIPLY_SUFFIX(endptr, *out, "xM", UINT64_C(1) << 20);
  MAYBE_MULTIPLY_SUFFIX(endptr, *out, "M", UINT64_C(1) << 20);
  MAYBE_MULTIPLY_SUFFIX(endptr, *out, "MB", UINT64_C(1000) * UINT64_C(1000));
  MAYBE_MULTIPLY_SUFFIX(endptr, *out, "K", UINT64_C(1) << 10);
  MAYBE_MULTIPLY_SUFFIX(endptr, *out, "kB", UINT64_C(1000));
  MAYBE_MULTIPLY_SUFFIX(endptr, *out, "b", UINT64_C(512));
  MAYBE_MULTIPLY_SUFFIX(endptr, *out, "w", UINT64_C(2));
  MAYBE_MULTIPLY_SUFFIX(endptr, *out, "c", UINT64_C(1));

done:
  fprintf(stderr, "Couldn't parse size string: %s\n", s);
  return -1;
}

struct DdOptions {
  bool use_count;
  size_t count;
  size_t input_bs;
  size_t input_skip;
  size_t output_bs;
  size_t output_seek;
  char input[PATH_MAX];
  char output[PATH_MAX];
  bool pad;
};

int ParseArgs(int argc, const char** argv, DdOptions* options) {
  while (argc > 1) {
    const char* arg = argv[1];
    if (PrefixMatch(&arg, "bs=")) {
      size_t size;
      if (ParseSize(arg, &size)) {
        return Usage();
      }
      options->input_bs = size;
      options->output_bs = size;
    } else if (PrefixMatch(&arg, "count=")) {
      if (ParseSize(arg, &options->count)) {
        return Usage();
      }
      options->use_count = true;
    } else if (PrefixMatch(&arg, "ibs=")) {
      if (ParseSize(arg, &options->input_bs)) {
        return Usage();
      }
    } else if (PrefixMatch(&arg, "obs=")) {
      if (ParseSize(arg, &options->output_bs)) {
        return Usage();
      }
    } else if (PrefixMatch(&arg, "seek=")) {
      if (ParseSize(arg, &options->output_seek)) {
        return Usage();
      }
    } else if (PrefixMatch(&arg, "skip=")) {
      if (ParseSize(arg, &options->input_skip)) {
        return Usage();
      }
    } else if (PrefixMatch(&arg, "if=")) {
      strncpy(options->input, arg, PATH_MAX);
      options->input[PATH_MAX - 1] = '\0';
    } else if (PrefixMatch(&arg, "of=")) {
      strncpy(options->output, arg, PATH_MAX);
      options->output[PATH_MAX - 1] = '\0';
    } else if (strcmp(arg, "conv=sync") == 0) {
      options->pad = true;
    } else {
      return Usage();
    }
    argc--;
    argv++;
  }
  return 0;
}

#define MIN(x, y) ((x) < (y) ? (x) : (y))
#define MAX(x, y) ((x) < (y) ? (y) : (x))

bool IsSkipBlockPath(std::string_view path) {
  // E.g. /dev/sys/platform/05:00:f/aml-raw_nand/nand/zircon-a/skip-block
  return cpp20::starts_with(path, "/dev") && cpp20::ends_with(path, "skip-block");
}

zx::result<fidl::ClientEnd<fuchsia_hardware_skipblock::SkipBlock>> GetSkipBlockClient(
    const char* path, size_t* block_size) {
  auto client = component::Connect<fuchsia_hardware_skipblock::SkipBlock>(path);
  if (client.is_error()) {
    return client.take_error();
  }
  auto res = fidl::WireCall(*client)->GetPartitionInfo();
  if (!res.ok()) {
    return zx::error(res.error().status());
  }
  if (res.value().status != ZX_OK) {
    return zx::error(res.value().status);
  }
  *block_size = res.value().partition_info.block_size_bytes;
  return zx::ok(std::move(*client));
}

int main(int argc, const char** argv) {
  DdOptions options = {};
  options.input_bs = 512;
  options.output_bs = 512;
  if (int r = ParseArgs(argc, argv, &options); r) {
    return r;
  }

  if (options.input_bs == 0 || options.output_bs == 0) {
    fprintf(stderr, "block sizes must be greater than zero\n");
    return -1;
  }

  options.input_skip *= options.input_bs;
  options.output_seek *= options.output_bs;
  size_t buf_size = MAX(options.output_bs, options.input_bs);

  // Input and output fds
  fbl::unique_fd in;
  fbl::unique_fd out;
  // Buffer to contain partially read data
  std::unique_ptr<char[]> buf;
  // Return code
  int r = EXIT_FAILURE;
  // Number of full records copied to/from target
  size_t records_in = 0;
  size_t records_out = 0;
  // Size of remaining "partial" transfer from input / to output.
  size_t record_in_partial = 0;
  size_t record_out_partial = 0;

  uint64_t sum_bytes_out = 0;
  // Logic for skip-block devices.
  zx::vmo vmo;
  fidl::ClientEnd<fuchsia_hardware_skipblock::SkipBlock> skipblock_in;
  size_t skipblock_size_in;
  fidl::ClientEnd<fuchsia_hardware_skipblock::SkipBlock> skipblock_out;
  size_t skipblock_size_out;
  zx_time_t start = 0, stop = 0;

  if (*options.input == '\0') {
    in.reset(STDIN_FILENO);
  } else {
    in.reset(open(options.input, O_RDONLY));
    if (!in) {
      fprintf(stderr, "Couldn't open input file %s : %d\n", options.input, errno);
      return EXIT_FAILURE;
    }
  }

  if (*options.output == '\0') {
    out.reset(STDOUT_FILENO);
  } else {
    out.reset(open(options.output, O_WRONLY | O_CREAT, S_IRUSR | S_IWUSR));
    if (!out) {
      fprintf(stderr, "Couldn't open output file %s : %d\n", options.output, errno);
      return EXIT_FAILURE;
    }
  }

  if (IsSkipBlockPath(options.input)) {
    zx::result result = GetSkipBlockClient(options.input, &skipblock_size_in);
    if (result.is_error()) {
      fprintf(stderr, "Failed to get skip-block client: %s\n", result.status_string());
      return EXIT_FAILURE;
    }
    if (options.input_bs % skipblock_size_in) {
      fprintf(stderr, "BS must be a multiple of %lu\n", skipblock_size_in);
      return EXIT_FAILURE;
    }
    skipblock_in = std::move(*result);
  }

  if (IsSkipBlockPath(options.output)) {
    zx::result result = GetSkipBlockClient(options.output, &skipblock_size_out);
    if (result.is_error()) {
      fprintf(stderr, "Failed to get skip-block client: %s\n", result.status_string());
      return EXIT_FAILURE;
    }
    if (options.output_bs % skipblock_size_out) {
      fprintf(stderr, "BS must be a multiple of %lu\n", skipblock_size_out);
      return EXIT_FAILURE;
    }
    skipblock_out = std::move(*result);
  }

  if (skipblock_in || skipblock_out) {
    if (zx::vmo::create(buf_size, 0, &vmo) != ZX_OK) {
      fprintf(stderr, "No memory\n");
      return EXIT_FAILURE;
    }
    if (zx_vmar_map(zx_vmar_root_self(), ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, 0, vmo.get(), 0,
                    buf_size, reinterpret_cast<zx_vaddr_t*>(&buf)) != ZX_OK) {
      fprintf(stderr, "Failed to map vmo\n");
      return EXIT_FAILURE;
    }
  } else {
    buf.reset(new char[buf_size]);
  }

  bool terminating = false;
  size_t rlen = 0;
  if (options.input_skip != 0 && !skipblock_in) {
    // Try seeking first; if that doesn't work, try reading to an input buffer.
    if (lseek(in.get(), static_cast<off_t>(options.input_skip), SEEK_SET) !=
        static_cast<off_t>(options.input_skip)) {
      while (options.input_skip) {
        if (read(in.get(), buf.get(), options.input_bs) != static_cast<ssize_t>(options.input_bs)) {
          fprintf(stderr, "Couldn't read from input\n");
          goto done;
        }
        options.input_skip -= options.input_bs;
      }
    }
  }

  if (options.output_seek != 0 && !skipblock_out) {
    if (lseek(out.get(), static_cast<off_t>(options.output_seek), SEEK_SET) !=
        static_cast<off_t>(options.output_seek)) {
      fprintf(stderr, "Failed to seek on output\n");
      goto done;
    }
  }

  if (MAX(options.input_bs, options.output_bs) % MIN(options.input_bs, options.output_bs) != 0) {
    // TODO(smklein): Implement this case, rather than returning an error
    fprintf(stderr, "Input and output block sizes must be multiples\n");
    goto done;
  }

  start = zx_clock_get_monotonic();
  while (true) {
    if (options.use_count && !options.count) {
      r = EXIT_SUCCESS;
      goto done;
    }

    // Read as much as we can (up to input_bs) into our target buffer
    if (skipblock_in) {
      zx::vmo dup;
      if (vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &dup) != ZX_OK) {
        fprintf(stderr, "Cannot duplicate handle\n");
        goto done;
      }
      const auto block_count = static_cast<uint32_t>(options.input_bs / skipblock_size_in);
      fuchsia_hardware_skipblock::wire::ReadWriteOperation op = {
          .vmo = std::move(dup),
          .vmo_offset = 0,
          .block = static_cast<uint32_t>((options.input_skip / skipblock_size_in) +
                                         (records_in * block_count)),
          .block_count = block_count,
      };

      auto res =
          fidl::WireCall<fuchsia_hardware_skipblock::SkipBlock>(skipblock_in)->Read(std::move(op));
      if (!res.ok()) {
        fprintf(stderr, "Read FIDL failure\n");
        goto done;
      }
      if (res.value().status != ZX_OK) {
        fprintf(stderr, "Read failed: %s\n", res.status_string());
        goto done;
      }
      records_in++;
      rlen += options.input_bs;
    } else {
      ssize_t rout = read(in.get(), buf.get(), options.input_bs);
      if (rout == static_cast<ssize_t>(options.input_bs)) {
        records_in++;
      } else {
        terminating = true;
        if (rout > 0) {
          if (options.pad) {
            memset(buf.get() + rout, 0, options.input_bs - rout);
            records_in++;
            rout = static_cast<ssize_t>(options.input_bs);
          } else {
            record_in_partial = rout;
          }
        }
      }
      if (rout > 0) {
        rlen += rout;
      }
    }
    if (options.use_count) {
      --options.count;
      if (options.count == 0) {
        terminating = true;
      }
    }

    // If we can (or should, due to impending termination), dump the read
    // buffer into the output file.
    if (rlen >= options.output_bs || terminating) {
      if (skipblock_out) {
        zx::vmo dup;
        if (vmo.duplicate(ZX_RIGHT_SAME_RIGHTS, &dup) != ZX_OK) {
          fprintf(stderr, "Cannot duplicate handle\n");
          goto done;
        }
        const uint32_t block_count = static_cast<uint32_t>(options.output_bs / skipblock_size_out);

        fuchsia_hardware_skipblock::wire::ReadWriteOperation op = {
            .vmo = std::move(dup),
            .vmo_offset = 0,
            .block = static_cast<uint32_t>((options.output_seek / skipblock_size_out) +
                                           (records_out * block_count)),
            .block_count = block_count,
        };

        auto res = fidl::WireCall<fuchsia_hardware_skipblock::SkipBlock>(skipblock_out)
                       ->Write(std::move(op));
        if (!res.ok()) {
          fprintf(stderr, "Write FIDL failure\n");
          goto done;
        }
        if (res.value().status != ZX_OK) {
          fprintf(stderr, "Write failed: %s\n", res.status_string());
          goto done;
        }
        records_out++;
      } else {
        size_t off = 0;
        while (off != rlen) {
          size_t wlen = MIN(options.output_bs, rlen - off);
          if (write(out.get(), buf.get() + off, wlen) != static_cast<ssize_t>(wlen)) {
            fprintf(stderr, "Couldn't write %zu bytes to output\n", wlen);
            goto done;
          }
          if (wlen == options.output_bs) {
            records_out++;
          } else {
            record_out_partial = wlen;
          }
          off += wlen;
        }
      }
      rlen = 0;
    }

    if (terminating) {
      r = EXIT_SUCCESS;
      goto done;
    }
  }

done:
  stop = zx_clock_get_monotonic();
  printf("%zu+%u records in\n", records_in, record_in_partial ? 1 : 0);
  printf("%zu+%u records out\n", records_out, record_out_partial ? 1 : 0);
  sum_bytes_out = records_out * options.output_bs + record_out_partial;
  if ((start != 0) && (stop > start)) {
    fprintf(stderr, "%zu bytes copied, %zu bytes/s\n", sum_bytes_out,
            sum_bytes_out * ZX_SEC(1) / (stop - start));
  } else {
    printf("%zu bytes copied\n", sum_bytes_out);
  }
  return r;
}
