// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <sys/mman.h>
#include <sys/stat.h>

#include <cstdio>
#include <set>
#include <string>
#include <string_view>
#include <type_traits>

#include <fbl/unique_fd.h>
#include <safemath/safe_math.h>
#include <src/lib/digest/merkle-tree.h>

#include "src/lib/fxl/command_line.h"
#include "src/lib/fxl/log_settings_command_line.h"
#include "src/storage/blobfs/compression/configs/chunked_compression_params.h"
#include "src/storage/tools/blobfs-compression/blobfs-compression.h"

namespace {

using blobfs::DeliveryBlobType;
using ::chunked_compression::CompressionParams;

const std::set<std::string_view> kCliOptions = {
    "source_file", "compressed_file", "type", "disable_size_alignment", "help", "verbose",
};

zx::result<DeliveryBlobType> DeliveryTypeFromString(const std::string& delivery_type_str) {
  using DeliveryBlobTypeRaw = std::underlying_type_t<DeliveryBlobType>;
  const std::set<DeliveryBlobTypeRaw> kSupportedBlobTypes = {
      static_cast<DeliveryBlobTypeRaw>(DeliveryBlobType::kType1),
  };
  const DeliveryBlobTypeRaw type_raw =
      safemath::checked_cast<DeliveryBlobTypeRaw>(std::stoul(delivery_type_str));
  if (kSupportedBlobTypes.find(type_raw) == kSupportedBlobTypes.cend()) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  return zx::ok(DeliveryBlobType{type_raw});
}

void usage(const char* fname) {
  fprintf(stderr, "Usage: %s [--option1=value --option2 ...]\n\n", fname);
  fprintf(
      stderr,
      "The tool will output the maximum possible compressed file size using the exact same \n"
      "compression implementation in blobfs. The merkle tree used here is a non-compact merkle \n"
      "tree as it contributes to a bigger size than a compact merkle tree.\n\n");
  fprintf(stderr, "Options:\n");
  fprintf(stderr, "--%s=/path/to/file\n    %s\n", "source_file",
          "(required) the file to be compressed.");
  fprintf(stderr, "--%s=/path/to/file\n    %s\n", "compressed_file",
          "(optional) the compressed file output path (override if existing). Unless --type is "
          "specified, will contain compressed data with zero-padding at the end to ensure the "
          "compressed file size matches the size in stdout.");
  fprintf(stderr, "--%s=TYPE\n    %s\n", "type",
          "(optional) If specified, uses specified type for size calculation, and will output "
          "blob in delivery format. Output is only compressed if space is saved. Supported types:"
          "\n\t1 - Type A: zstd-chunked, default compression level");
  fprintf(stderr, "--%s\n    %s\n", "disable_size_alignment",
          "Do not align compressed output with block size. Incompatible with --type.");
  fprintf(stderr, "--%s\n    %s\n", "help", "print this usage message.");
  fprintf(stderr, "--%s\n    %s\n", "verbose", "show debugging information.");
}

// Truncates |fd| to |write_size|, and mmaps the file for writing.
// Returns the mapped buffer in |out_write_buf| of length |write_size|.
// This method can fail only with user-input-irrelevant errors.
zx_status_t MapFileForWriting(const fbl::unique_fd& fd, const char* file, size_t write_size,
                              uint8_t** out_write_buf) {
  off_t trunc_size;
  if (!safemath::MakeCheckedNum<size_t>(write_size).Cast<off_t>().AssignIfValid(&trunc_size)) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (ftruncate(fd.get(), trunc_size)) {
    fprintf(stderr, "Failed to truncate '%s': %s\n", file, strerror(errno));
    return ZX_ERR_NO_SPACE;
  }

  void* data = nullptr;
  if (write_size > 0) {
    data = mmap(nullptr, write_size, PROT_READ | PROT_WRITE, MAP_SHARED, fd.get(), 0);
    if (data == MAP_FAILED) {
      fprintf(stderr, "mmap failed: %s\n", strerror(errno));
      return ZX_ERR_NO_MEMORY;
    }
  }

  *out_write_buf = static_cast<uint8_t*>(data);
  return ZX_OK;
}

// Mmaps the |fd| for reading.
// Returns the  size of the file in |out_size|, and the managed FD in |out_fd|.
// This method can fail only with user-input-irrelevant errors.
zx_status_t MapFileForReading(const fbl::unique_fd& fd, const uint8_t** out_buf, size_t* out_size) {
  struct stat info;
  fstat(fd.get(), &info);
  size_t size = info.st_size;

  void* data = nullptr;
  if (size > 0) {
    data = mmap(nullptr, size, PROT_READ, MAP_SHARED, fd.get(), 0);
    if (data == MAP_FAILED) {
      fprintf(stderr, "mmap failed: %s\n", strerror(errno));
      return ZX_ERR_NO_MEMORY;
    }
  }

  *out_buf = static_cast<uint8_t*>(data);
  *out_size = size;
  return ZX_OK;
}

zx::result<> WriteDataToFile(const fbl::unique_fd& fd, cpp20::span<const uint8_t> data) {
  size_t written_bytes = 0;
  ssize_t write_result = 0;
  while (written_bytes < data.size_bytes()) {
    write_result = write(fd.get(), data.data() + written_bytes, data.size_bytes() - written_bytes);
    if (write_result < 0) {
      fprintf(stderr, "Failed to write blob: %s\n", strerror(errno));
      return zx::error(ZX_ERR_IO);
    }
    written_bytes += write_result;
  }
  return zx::ok();
}

}  // namespace

int main(int argc, char** argv) {
  const auto cl = fxl::CommandLineFromArgcArgv(argc, argv);
  if (!fxl::SetLogSettingsFromCommandLine(cl)) {
    return 1;
  }

  const bool verbose = cl.HasOption("verbose", nullptr);
  if (verbose) {
    printf("Received flags:\n");
    for (const auto& option : cl.options()) {
      printf("  %s = \"%s\"\n", option.name.c_str(), option.value.c_str());
    }
    printf("\n");
  }

  // Check unknown input options.
  bool printHelp = cl.HasOption("help");
  for (const auto& option : cl.options()) {
    if (kCliOptions.find(option.name) == kCliOptions.end()) {
      fprintf(stderr, "Error: unknown option \"%s\".\n", option.name.c_str());
      printHelp = true;
    }
  }
  if (printHelp) {
    usage(argv[0]);
    return 0;
  }

  blobfs_compress::CompressionCliOptionStruct options;

  // Parse required args.
  if (!cl.HasOption("source_file")) {
    fprintf(stderr, "Error: missing required option: --source_file\n");
    usage(argv[0]);
    return ZX_ERR_INVALID_ARGS;
  }
  ZX_ASSERT(cl.GetOptionValue("source_file", &options.source_file));
  options.source_file_fd.reset(open(options.source_file.c_str(), O_RDONLY));

  // Parse optional args.
  if (cl.HasOption("disable_size_alignment") && cl.HasOption("type")) {
    usage(argv[0]);
    return ZX_ERR_INVALID_ARGS;
  }
  options.disable_size_alignment = cl.HasOption("disable_size_alignment");

  if (cl.HasOption("type")) {
    if (!cl.HasOption("compressed_file")) {
      fprintf(stderr, "Error: --compressed-file must be specified with --type.\n");
      usage(argv[0]);
      return ZX_ERR_INVALID_ARGS;
    }
    std::string delivery_blob_type_option;
    ZX_ASSERT(cl.GetOptionValue("type", &delivery_blob_type_option));
    zx::result delivery_type = DeliveryTypeFromString(delivery_blob_type_option);
    if (delivery_type.is_error()) {
      fprintf(stderr,
              "Error: unrecognized or invalid value for --type. See usage "
              "for list of supported delivery types.\n");
      usage(argv[0]);
      return delivery_type.error_value();
    }
    options.type = delivery_type.value();
  }

  if (cl.HasOption("compressed_file")) {
    ZX_ASSERT(cl.GetOptionValue("compressed_file", &options.compressed_file));
    options.compressed_file_fd.reset(
        open(options.compressed_file.c_str(), O_RDWR | O_CREAT | O_TRUNC, 0644));
  }

  zx_status_t error_code = blobfs_compress::ValidateCliOptions(options);
  if (error_code) {
    usage(argv[0]);
    return error_code;
  }

  const uint8_t* src_data;
  size_t src_size;
  error_code = MapFileForReading(options.source_file_fd, &src_data, &src_size);
  if (error_code) {
    return error_code;
  }

  // We need to generate a delivery blob.
  if (options.type.has_value()) {
    ZX_ASSERT(!options.compressed_file.empty() && options.compressed_file_fd.is_valid());
    const zx::result delivery_blob =
        blobfs_compress::GenerateDeliveryBlob({src_data, src_size}, *options.type);
    if (delivery_blob.is_error()) {
      fprintf(stderr, "Error generating delivery blob.\n");
      return delivery_blob.error_value();
    }
    const zx::result write_result =
        WriteDataToFile(options.compressed_file_fd, {delivery_blob->data(), delivery_blob->size()});
    return write_result.status_value();
  }

  uint8_t* dest_data = nullptr;
  CompressionParams params = blobfs::GetDefaultChunkedCompressionParams(src_size);
  if (!options.compressed_file.empty()) {
    const size_t dest_buffer_size =
        params.ComputeOutputSizeLimit(src_size) +
        digest::CalculateMerkleTreeSize(src_size, digest::kDefaultNodeSize, false);
    error_code = MapFileForWriting(options.compressed_file_fd, options.compressed_file.c_str(),
                                   dest_buffer_size, &dest_data);
    if (error_code) {
      return error_code;
    }
  }

  // Compress the blob and output compressed size, optionally writing data into the mapped buffer.

  size_t dest_size;
  if (blobfs_compress::BlobfsCompress(src_data, src_size, dest_data, &dest_size, params, options)) {
    return ZX_ERR_INTERNAL;
  }

  if (!options.compressed_file.empty()) {
    off_t trunc_size;
    if (!safemath::MakeCheckedNum<size_t>(dest_size).Cast<off_t>().AssignIfValid(&trunc_size)) {
      return ZX_ERR_OUT_OF_RANGE;
    }

    ftruncate(options.compressed_file_fd.get(), trunc_size);
  }
  return ZX_OK;
}
