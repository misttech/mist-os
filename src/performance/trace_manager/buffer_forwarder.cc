// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/performance/trace_manager/buffer_forwarder.h"

#include <lib/syslog/cpp/macros.h>
#include <lib/trace-engine/fields.h>

namespace tracing {

TransferStatus BufferForwarder::WriteMagicNumberRecord() const {
  size_t num_words = 1u;
  uint64_t record = trace::MagicNumberRecordFields::Type::Make(
                        trace::ToUnderlyingType(trace::RecordType::kMetadata)) |
                    trace::MagicNumberRecordFields::RecordSize::Make(num_words) |
                    trace::MagicNumberRecordFields::MetadataType::Make(
                        trace::ToUnderlyingType(trace::MetadataType::kTraceInfo)) |
                    trace::MagicNumberRecordFields::TraceInfoType::Make(
                        trace::ToUnderlyingType(trace::TraceInfoType::kMagicNumber)) |
                    trace::MagicNumberRecordFields::Magic::Make(trace::kMagicValue);
  return WriteBuffer(reinterpret_cast<uint8_t*>(&record), trace::WordsToBytes(num_words));
}

TransferStatus BufferForwarder::WriteProviderInfoRecord(uint32_t provider_id,
                                                        const std::string& name) const {
  size_t num_words = 1u + trace::BytesToWords(trace::Pad(name.size()));
  std::vector<uint64_t> record(num_words);
  record[0] = trace::ProviderInfoMetadataRecordFields::Type::Make(
                  trace::ToUnderlyingType(trace::RecordType::kMetadata)) |
              trace::ProviderInfoMetadataRecordFields::RecordSize::Make(num_words) |
              trace::ProviderInfoMetadataRecordFields::MetadataType::Make(
                  trace::ToUnderlyingType(trace::MetadataType::kProviderInfo)) |
              trace::ProviderInfoMetadataRecordFields::Id::Make(provider_id) |
              trace::ProviderInfoMetadataRecordFields::NameLength::Make(name.size());
  memcpy(&record[1], name.c_str(), name.size());
  return WriteBuffer(reinterpret_cast<uint8_t*>(record.data()), trace::WordsToBytes(num_words));
}

TransferStatus BufferForwarder::WriteProviderSectionRecord(uint32_t provider_id) const {
  size_t num_words = 1u;
  uint64_t record = trace::ProviderSectionMetadataRecordFields::Type::Make(
                        trace::ToUnderlyingType(trace::RecordType::kMetadata)) |
                    trace::ProviderSectionMetadataRecordFields::RecordSize::Make(num_words) |
                    trace::ProviderSectionMetadataRecordFields::MetadataType::Make(
                        trace::ToUnderlyingType(trace::MetadataType::kProviderSection)) |
                    trace::ProviderSectionMetadataRecordFields::Id::Make(provider_id);
  return WriteBuffer(reinterpret_cast<uint8_t*>(&record), trace::WordsToBytes(num_words));
}

TransferStatus BufferForwarder::WriteProviderBufferOverflowEvent(uint32_t provider_id) const {
  size_t num_words = 1u;
  uint64_t record = trace::ProviderEventMetadataRecordFields::Type::Make(
                        trace::ToUnderlyingType(trace::RecordType::kMetadata)) |
                    trace::ProviderEventMetadataRecordFields::RecordSize::Make(num_words) |
                    trace::ProviderEventMetadataRecordFields::MetadataType::Make(
                        trace::ToUnderlyingType(trace::MetadataType::kProviderEvent)) |
                    trace::ProviderEventMetadataRecordFields::Id::Make(provider_id) |
                    trace::ProviderEventMetadataRecordFields::Event::Make(
                        trace::ToUnderlyingType(trace::ProviderEventType::kBufferOverflow));
  return WriteBuffer(reinterpret_cast<uint8_t*>(&record), trace::WordsToBytes(num_words));
}

TransferStatus BufferForwarder::WriteBuffer(const void* buffer, size_t len) const {
  auto data = reinterpret_cast<const uint8_t*>(buffer);
  size_t offset = 0;
  while (offset < len) {
    zx_status_t status = ZX_OK;
    size_t actual = 0;
    if ((status = destination_.write(0u, data + offset, len - offset, &actual)) < 0) {
      if (status == ZX_ERR_SHOULD_WAIT) {
        zx_signals_t pending = 0;
        status = destination_.wait_one(ZX_SOCKET_WRITABLE | ZX_SOCKET_PEER_CLOSED,
                                       zx::time::infinite(), &pending);
        if (status < 0) {
          FX_LOGS(ERROR) << "Wait on socket failed: " << status;
          return TransferStatus::kWriteError;
        }

        if (pending & ZX_SOCKET_WRITABLE)
          continue;

        if (pending & ZX_SOCKET_PEER_CLOSED) {
          FX_LOGS(ERROR) << "Peer closed while writing to socket";
          return TransferStatus::kReceiverDead;
        }
      }

      return TransferStatus::kWriteError;
    }
    offset += actual;
  }

  return TransferStatus::kComplete;
}
}  // namespace tracing
