// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/pseudo_file.h"

#include <fidl/fuchsia.io/cpp/wire.h>

#include <string_view>
#include <utility>

#include "src/storage/lib/vfs/cpp/vfs_types.h"

namespace fio = fuchsia_io;

namespace fs {

PseudoFile::PseudoFile(ReadHandler read_handler, WriteHandler write_handler)
    : read_handler_(std::move(read_handler)), write_handler_(std::move(write_handler)) {}

PseudoFile::~PseudoFile() = default;

fuchsia_io::NodeProtocolKinds PseudoFile::GetProtocols() const {
  return fuchsia_io::NodeProtocolKinds::kFile;
}

bool PseudoFile::ValidateRights(fuchsia_io::Rights rights) const {
  if ((rights & fuchsia_io::Rights::kReadBytes) && !read_handler_) {
    return false;
  }
  if ((rights & fuchsia_io::Rights::kWriteBytes) && !write_handler_) {
    return false;
  }
  // Executable pseudo-files are not supported, thus we prevent it from being opened with
  // OPEN_RIGHT_EXECUTABLE (since even if GetBackingMemory was supported, there is no way of
  // creating an executable VMO without a VMEX, which poses potential security issues).
  if (rights & fuchsia_io::Rights::kExecute) {
    return false;
  }
  return true;
}

zx_status_t PseudoFile::GetAttributes(VnodeAttributes* attr) {
  *attr = VnodeAttributes();
  attr->mode = V_TYPE_FILE;
  if (read_handler_)
    attr->mode |= V_IRUSR;
  if (write_handler_)
    attr->mode |= V_IWUSR;
  attr->inode = fio::wire::kInoUnknown;
  attr->link_count = 1;
  return ZX_OK;
}

BufferedPseudoFile::BufferedPseudoFile(ReadHandler read_handler, WriteHandler write_handler,
                                       size_t input_buffer_capacity)
    : PseudoFile(std::move(read_handler), std::move(write_handler)),
      input_buffer_capacity_(input_buffer_capacity) {}

BufferedPseudoFile::~BufferedPseudoFile() = default;

zx_status_t BufferedPseudoFile::OpenNode(fbl::RefPtr<Vnode>* out_redirect) {
  fbl::String output;
  if (read_handler_) {
    zx_status_t status = read_handler_(&output);
    if (status != ZX_OK) {
      return status;
    }
  }

  *out_redirect = fbl::MakeRefCounted<Content>(fbl::RefPtr(this), std::move(output));
  return ZX_OK;
}

BufferedPseudoFile::Content::Content(fbl::RefPtr<BufferedPseudoFile> file, fbl::String output)
    : file_(std::move(file)), output_(std::move(output)) {}

BufferedPseudoFile::Content::~Content() { delete[] input_data_; }

fuchsia_io::NodeProtocolKinds BufferedPseudoFile::Content::GetProtocols() const {
  return fuchsia_io::NodeProtocolKinds::kFile;
}

zx_status_t BufferedPseudoFile::Content::CloseNode() {
  if (file_->write_handler_) {
    return file_->write_handler_(std::string_view(input_data_, input_length_));
  }
  return ZX_OK;
}

zx_status_t BufferedPseudoFile::Content::GetAttributes(fs::VnodeAttributes* a) {
  zx_status_t status = file_->GetAttributes(a);
  a->content_size = output_.size();
  return status;
}

zx_status_t BufferedPseudoFile::Content::Read(void* data, size_t length, size_t offset,
                                              size_t* out_actual) {
  if (length == 0u || offset >= output_.length()) {
    *out_actual = 0u;
    return ZX_OK;
  }
  length = std::min(length, output_.length() - offset);
  memcpy(data, output_.data() + offset, length);
  *out_actual = length;
  return ZX_OK;
}

zx_status_t BufferedPseudoFile::Content::Write(const void* data, size_t length, size_t offset,
                                               size_t* out_actual) {
  if (length == 0u) {
    *out_actual = 0u;
    return ZX_OK;
  }
  if (offset >= file_->input_buffer_capacity_) {
    return ZX_ERR_NO_SPACE;
  }
  length = std::min(length, file_->input_buffer_capacity_ - offset);
  if (offset + length > input_length_) {
    SetInputLength(offset + length);
  }
  memcpy(input_data_ + offset, data, length);
  *out_actual = length;
  return ZX_OK;
}

zx_status_t BufferedPseudoFile::Content::Append(const void* data, size_t length, size_t* out_end,
                                                size_t* out_actual) {
  zx_status_t status = Write(data, length, input_length_, out_actual);
  if (status == ZX_OK) {
    *out_end = input_length_;
  }
  return status;
}

zx_status_t BufferedPseudoFile::Content::Truncate(size_t length) {
  if (length > file_->input_buffer_capacity_) {
    return ZX_ERR_NO_SPACE;
  }

  size_t old_length = input_length_;
  SetInputLength(length);
  if (length > old_length) {
    memset(input_data_ + old_length, 0, length - old_length);
  }
  return ZX_OK;
}

void BufferedPseudoFile::Content::SetInputLength(size_t length) {
  ZX_DEBUG_ASSERT(length <= file_->input_buffer_capacity_);

  if (input_data_ == nullptr && length != 0u) {
    input_data_ = new char[file_->input_buffer_capacity_];
  }
  input_length_ = length;
}

UnbufferedPseudoFile::UnbufferedPseudoFile(ReadHandler read_handler, WriteHandler write_handler)
    : PseudoFile(std::move(read_handler), std::move(write_handler)) {}

UnbufferedPseudoFile::~UnbufferedPseudoFile() = default;

fuchsia_io::NodeProtocolKinds UnbufferedPseudoFile::Content::GetProtocols() const {
  return fuchsia_io::NodeProtocolKinds::kFile;
}

zx_status_t UnbufferedPseudoFile::OpenNode(fbl::RefPtr<Vnode>* out_redirect) {
  *out_redirect = fbl::MakeRefCounted<Content>(fbl::RefPtr(this));
  return ZX_OK;
}

UnbufferedPseudoFile::Content::Content(fbl::RefPtr<UnbufferedPseudoFile> file)
    : file_(std::move(file)), truncated_since_last_successful_write_(false) {}

UnbufferedPseudoFile::Content::~Content() = default;

zx_status_t UnbufferedPseudoFile::Content::OpenNode(fbl::RefPtr<Vnode>* out_redirect) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t UnbufferedPseudoFile::Content::CloseNode() {
  if (file_->write_handler_ && truncated_since_last_successful_write_) {
    return file_->write_handler_(std::string_view());
  }
  return ZX_OK;
}

zx_status_t UnbufferedPseudoFile::Content::GetAttributes(fs::VnodeAttributes* a) {
  return file_->GetAttributes(a);
}

zx_status_t UnbufferedPseudoFile::Content::Read(void* data, size_t length, size_t offset,
                                                size_t* out_actual) {
  if (offset != 0u) {
    // If the offset is non-zero, we assume the client already read the property. Simulate end of
    // file.
    *out_actual = 0u;
    return ZX_OK;
  }

  fbl::String output;
  zx_status_t status = file_->read_handler_(&output);
  if (status == ZX_OK) {
    length = std::min(length, output.length());
    memcpy(data, output.data(), length);
    *out_actual = length;
  }
  return status;
}

zx_status_t UnbufferedPseudoFile::Content::Write(const void* data, size_t length, size_t offset,
                                                 size_t* out_actual) {
  if (offset != 0u) {
    // If the offset is non-zero, we assume the client already wrote the property. Simulate an
    // inability to write additional data.
    return ZX_ERR_NO_SPACE;
  }

  zx_status_t status =
      file_->write_handler_(std::string_view(static_cast<const char*>(data), length));
  if (status == ZX_OK) {
    truncated_since_last_successful_write_ = false;
    *out_actual = length;
  }
  return status;
}

zx_status_t UnbufferedPseudoFile::Content::Append(const void* data, size_t length, size_t* out_end,
                                                  size_t* out_actual) {
  zx_status_t status = Write(data, length, 0u, out_actual);
  if (status == ZX_OK) {
    *out_end = length;
  }
  return status;
}

zx_status_t UnbufferedPseudoFile::Content::Truncate(size_t length) {
  if (length != 0u) {
    return ZX_ERR_INVALID_ARGS;
  }

  truncated_since_last_successful_write_ = true;
  return ZX_OK;
}

}  // namespace fs
