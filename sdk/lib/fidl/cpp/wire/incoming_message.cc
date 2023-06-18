// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fidl/cpp/wire/incoming_message.h>
#include <lib/fidl/txn_header.h>

#ifdef __Fuchsia__
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#else
#include <lib/fidl/cpp/wire/internal/transport_channel_host.h>
#endif  // __Fuchsia__

namespace fidl {

EncodedMessage EncodedMessage::Create(cpp20::span<uint8_t> bytes) {
  return EncodedMessage(nullptr, bytes, nullptr, nullptr, 0);
}

EncodedMessage EncodedMessage::Create(cpp20::span<uint8_t> bytes, zx_handle_t* handles,
                                      fidl_channel_handle_metadata_t* handle_metadata,
                                      uint32_t handle_actual) {
  return EncodedMessage(&internal::ChannelTransport::VTable, bytes, handles,
                        reinterpret_cast<fidl_handle_metadata_t*>(handle_metadata), handle_actual);
}

std::pair<cpp20::span<uint8_t>, cpp20::span<fidl_handle_t>> EncodedMessage::Release() && {
  ZX_ASSERT(transport_vtable_->type == FIDL_TRANSPORT_TYPE_CHANNEL);
  cpp20::span bytes{reinterpret_cast<uint8_t*>(message_.bytes), message_.num_bytes};
  cpp20::span handles{message_.handles, message_.num_handles};
  std::move(*this).ReleaseHandles();
  return {bytes, handles};
}

EncodedMessage::~EncodedMessage() { std::move(*this).CloseHandles(); }

void EncodedMessage::CloseHandles() && {
  if (transport_vtable_) {
    transport_vtable_->encoding_configuration->close_many(handles(), num_handles());
  }
  std::move(*this).ReleaseHandles();
}

EncodedMessage::EncodedMessage(const internal::TransportVTable* transport_vtable,
                               cpp20::span<uint8_t> bytes, fidl_handle_t* handles,
                               fidl_handle_metadata_t* handle_metadata, uint32_t handle_actual)
    : transport_vtable_(transport_vtable),
      message_(fidl_incoming_msg_t{
          .bytes = bytes.data(),
          .handles = handles,
          .handle_metadata = handle_metadata,
          .num_bytes = static_cast<uint32_t>(bytes.size()),
          .num_handles = handle_actual,
      }) {
  ZX_DEBUG_ASSERT(bytes.size() < std::numeric_limits<uint32_t>::max());
}

IncomingHeaderAndMessage IncomingHeaderAndMessage::FromEncodedCMessage(
    const fidl_incoming_msg_t& c_msg) {
  ZX_DEBUG_ASSERT(c_msg.num_bytes >= sizeof(fidl_message_header_t));
  return IncomingHeaderAndMessage(&internal::ChannelTransport::VTable,
                                  reinterpret_cast<uint8_t*>(c_msg.bytes), c_msg.num_bytes,
                                  c_msg.handles, c_msg.handle_metadata, c_msg.num_handles);
}

IncomingHeaderAndMessage::~IncomingHeaderAndMessage() = default;

fidl_epitaph_t* IncomingHeaderAndMessage::maybe_epitaph() const {
  ZX_DEBUG_ASSERT(ok());
  if (unlikely(header()->ordinal == kFidlOrdinalEpitaph)) {
    return reinterpret_cast<fidl_epitaph_t*>(bytes());
  }
  return nullptr;
}

fidl_incoming_msg_t IncomingHeaderAndMessage::ReleaseToEncodedCMessage() && {
  ZX_DEBUG_ASSERT_MSG(status() == ZX_OK, "%s", status_string());
  fidl_handle_t* handles = body_.handles();
  uint32_t num_handles = body_.num_handles();
  fidl_handle_metadata_t* handle_metadata = body_.raw_handle_metadata();
  std::move(body_).ReleaseHandles();
  return {
      .bytes = bytes_.data(),
      .handles = handles,
      .handle_metadata = handle_metadata,
      .num_bytes = static_cast<uint32_t>(bytes_.size()),
      .num_handles = num_handles,
  };
}

void IncomingHeaderAndMessage::CloseHandles() && { std::move(body_).CloseHandles(); }

EncodedMessage IncomingHeaderAndMessage::SkipTransactionHeader() && { return std::move(body_); }

IncomingHeaderAndMessage::IncomingHeaderAndMessage(const fidl::Status& failure)
    : fidl::Status(failure), body_(EncodedMessage::Create({})) {
  ZX_DEBUG_ASSERT(failure.status() != ZX_OK);
}

IncomingHeaderAndMessage::IncomingHeaderAndMessage(
    const internal::TransportVTable* transport_vtable, uint8_t* bytes, uint32_t byte_actual,
    fidl_handle_t* handles, fidl_handle_metadata_t* handle_metadata, uint32_t handle_actual)
    : fidl::Status(fidl::Status::Ok()),
      bytes_(cpp20::span{bytes, byte_actual}),
      body_(bytes_.size() >= sizeof(fidl_message_header_t)
                ? EncodedMessage(transport_vtable, bytes_.subspan(sizeof(fidl_message_header_t)),
                                 handles, handle_metadata, handle_actual)
                : EncodedMessage::Create({})) {
  ValidateHeader();
}

void IncomingHeaderAndMessage::ValidateHeader() {
  if (byte_actual() < sizeof(fidl_message_header_t)) {
    return SetStatus(fidl::Status::UnexpectedMessage(ZX_ERR_INVALID_ARGS,
                                                     ::fidl::internal::kErrorInvalidHeader));
  }

  auto* hdr = header();
  zx_status_t status = fidl_validate_txn_header(hdr);
  if (status != ZX_OK) {
    return SetStatus(
        fidl::Status::UnexpectedMessage(status, ::fidl::internal::kErrorInvalidHeader));
  }

  // See
  // https://fuchsia.dev/fuchsia-src/contribute/governance/rfcs/0053_epitaphs?hl=en#wire_format
  if (unlikely(maybe_epitaph())) {
    if (hdr->txid != 0) {
      return SetStatus(fidl::Status::UnexpectedMessage(ZX_ERR_INVALID_ARGS,
                                                       ::fidl::internal::kErrorInvalidHeader));
    }
  }
}

}  // namespace fidl
