// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fidl/fidl.serversuite/cpp/natural_types.h"
#include "lib/fidl/cpp/unified_messaging.h"
#include "src/tests/fidl/server_suite/harness/harness.h"
#include "src/tests/fidl/server_suite/harness/ordinals.h"

using namespace channel_util;
using namespace fidl_serversuite;

namespace server_suite {

namespace {

const uint32_t kVectorHeaderSize = 16;
const uint32_t kUnionOrdinalAndEnvelopeSize = 16;
const uint32_t kDefaultElementsCount = kHandleCarryingElementsCount - 1;
const zx_rights_t kExpectedOverflowBufferRights =
    ZX_RIGHT_GET_PROPERTY | ZX_RIGHT_READ | ZX_RIGHT_TRANSFER | ZX_RIGHT_WAIT | ZX_RIGHT_INSPECT;
const zx_handle_info_t kExpectedOverflowBufferHandleInfo = zx_handle_info_t{
    .type = ZX_OBJ_TYPE_VMO,
    .rights = kExpectedOverflowBufferRights,
};

// While this is technically equivalent to just using `kByteOverflow` alone since `kStrictMethod`
// is 0, defining this constant makes the intent clearer.
const fidl::MessageDynamicFlags kStrictMethodAndByteOverflow =
    fidl::MessageDynamicFlags::kStrictMethod | fidl::MessageDynamicFlags::kByteOverflow;
const fidl::MessageDynamicFlags kFlexibleMethodAndByteOverflow =
    fidl::MessageDynamicFlags::kFlexibleMethod | fidl::MessageDynamicFlags::kByteOverflow;

Bytes populate_unset_handles_true() { return u64(1); }
Bytes populate_unset_handles_false() { return u64(0); }

// An encode test has three interesting properties that we want to validate: the attached handle
// state, the bytes in the channel message itself, and the existence and contents of the overflow
// buffer that may or may not be attached. Every "good" test case involving the
// |UnboundedMaybeLargeResource| FIDL type will need to be checked against this struct.
struct Expected {
  HandleInfos handle_infos;
  Bytes channel_bytes;
  std::optional<Bytes> vmo_bytes;
};

// Because |UnboundedMaybeLargeResource| is used so widely, and needs to have many parts (handles,
// VMO-stored data, etc) assembled just so in a variety of configurations (small/large with 0, 63,
// or 64 handles, plus all manner of mis-encodings), this helper struct keeps track of all of the
// bookkeeping necessary when building an |UnboundedMaybeLargeResource| of a certain shape.
class UnboundedMaybeLargeResourceWriter {
 public:
  enum class HandlePresence {
    kAbsent = 0,
    kPresent = 1,
  };

  using ByteVectorSize = size_t;
  UnboundedMaybeLargeResourceWriter(UnboundedMaybeLargeResourceWriter&&) = default;
  UnboundedMaybeLargeResourceWriter& operator=(UnboundedMaybeLargeResourceWriter&&) = default;
  UnboundedMaybeLargeResourceWriter(const UnboundedMaybeLargeResourceWriter&) = delete;
  UnboundedMaybeLargeResourceWriter& operator=(const UnboundedMaybeLargeResourceWriter&) = delete;

  // The first argument, |num_filled|, is a pair that specifies the number of entries in the
  // |elements| array that should be set to non-empty vectors, with the other number in the pair
  // specifying the number of bytes in each such vector. The second argument, |num_handles|,
  // specifies the number of |elements| that should have a present handle. For instance, the
  // constructor call |UnboundedMaybeLargeResourceWriter({30, 1000}, 20)| would produce an
  // `elements` array whose first 20 entries have 1000 bytes and a present handle, 10 more entries
  // that have 1000 bytes and an absent handle, with the final 34 entries containing both absent
  // byte vectors and absent handles.
  //
  // Generally speaking, tests will be clearer and more readable if users create a descriptively
  // named static builder on this class for their specific case (ex:
  // |LargestSmallMessage64Handles|).
  UnboundedMaybeLargeResourceWriter(std::pair<size_t, ByteVectorSize> num_filled,
                                    uint8_t num_handles) {
    for (size_t i = 0; i < kHandleCarryingElementsCount; i++) {
      byte_vector_sizes[i] = 0;
      handles[i] = HandlePresence::kAbsent;
      if (i < num_filled.first) {
        byte_vector_sizes[i] = num_filled.second;
        if (num_handles > i) {
          handles[i] = HandlePresence::kPresent;
        } else {
          handles[i] = HandlePresence::kAbsent;
        }
      }
    }
  }

  static UnboundedMaybeLargeResourceWriter SmallMessageAnd64Handles() {
    auto writer = UnboundedMaybeLargeResourceWriter(
        {kDefaultElementsCount, kFirst63ElementsByteVectorSize}, kHandleCarryingElementsCount);
    writer.byte_vector_sizes[kDefaultElementsCount] = kSmallLastElementByteVectorSize;
    return writer;
  }

  static UnboundedMaybeLargeResourceWriter LargeMessageAnd63Handles() {
    auto writer = UnboundedMaybeLargeResourceWriter(
        {kDefaultElementsCount, kFirst63ElementsByteVectorSize}, kDefaultElementsCount);
    writer.byte_vector_sizes[kDefaultElementsCount] = kLargeLastElementByteVectorSize;
    return writer;
  }

  static UnboundedMaybeLargeResourceWriter LargeMessageAnd0Handles() {
    auto writer = UnboundedMaybeLargeResourceWriter(
        {kDefaultElementsCount, kFirst63ElementsByteVectorSize}, 0);
    writer.byte_vector_sizes[kDefaultElementsCount] = kLargeLastElementByteVectorSize;
    return writer;
  }

  void WriteSmallMessageForDecode(Channel& client, Bytes header) {
    WriteSmallMessage(client, header);
  }

  void WriteSmallMessageForEncode(Channel& client, Bytes header, Bytes populate_unset_handles,
                                  Expected* out_expected) {
    WriteSmallMessage(client, header, populate_unset_handles, out_expected);
  }

  void WriteLargeMessageForDecode(Channel& client, Bytes header) {
    WriteLargeMessage(client, header);
  }

  void WriteLargeMessageForEncode(Channel& client, Bytes header, Bytes populate_unset_handles,
                                  Expected* out_expected) {
    WriteLargeMessage(client, header, populate_unset_handles, out_expected);
  }

 private:
  Bytes BuildPayload() {
    std::vector<Bytes> payload_bytes;
    // Add the inline portions of each |element| entry.
    for (size_t i = 0; i < kHandleCarryingElementsCount; i++) {
      payload_bytes.push_back(
          {vector_header(byte_vector_sizes[i]),
           handles[i] == HandlePresence::kPresent ? handle_present() : handle_absent(),
           padding(4)});
    }

    // Now do all out of line portions as well.
    for (size_t i = 0; i < kHandleCarryingElementsCount; i++) {
      if (byte_vector_sizes[i] > 0) {
        payload_bytes.push_back(repeat(kSomeByte).times(byte_vector_sizes[i]));
        if (byte_vector_sizes[i] % FIDL_ALIGNMENT != 0) {
          payload_bytes.push_back(
              padding(FIDL_ALIGNMENT - (byte_vector_sizes[i] % FIDL_ALIGNMENT)));
        }
      }
    }

    return Bytes(payload_bytes);
  }

  HandleDispositions BuildHandleDispositions() {
    HandleDispositions dispositions;
    for (const auto& maybe_handle : handles) {
      if (maybe_handle == HandlePresence::kPresent) {
        zx::event event;
        zx::event::create(0, &event);
        dispositions.push_back(zx_handle_disposition_t{
            .handle = event.release(),
            .type = ZX_OBJ_TYPE_EVENT,
            .rights = ZX_DEFAULT_EVENT_RIGHTS,
        });
      }
    }
    return dispositions;
  }

  HandleInfos BuildHandleInfos() {
    HandleInfos infos;
    for (const auto& maybe_handle : handles) {
      if (maybe_handle == HandlePresence::kPresent) {
        infos.push_back(zx_handle_info_t{
            .type = ZX_OBJ_TYPE_EVENT,
            .rights = ZX_DEFAULT_EVENT_RIGHTS,
        });
      }
    }
    return infos;
  }

  void WriteSmallMessage(Channel& client, Bytes header, Bytes populate_unset_handles = Bytes(),
                         Expected* out_expected = nullptr) {
    Bytes payload = BuildPayload();
    ZX_ASSERT_MSG(sizeof(fidl_message_header_t) + payload.size() + populate_unset_handles.size() <=
                      ZX_CHANNEL_MAX_MSG_BYTES,
                  "attempted to write large message using small message writer");

    if (out_expected != nullptr) {
      *out_expected = Expected{
          .handle_infos = BuildHandleInfos(),
          .channel_bytes = Bytes({header, payload}),
          .vmo_bytes = std::nullopt,
      };
    }

    Bytes bytes_in = Bytes({header, populate_unset_handles, payload});
    ASSERT_OK(client.write(bytes_in, BuildHandleDispositions()));
  }

  void WriteLargeMessage(Channel& client, Bytes header, Bytes populate_unset_handles = Bytes(),
                         Expected* out_expected = nullptr) {
    Bytes payload = BuildPayload();
    ZX_ASSERT_MSG(sizeof(fidl_message_header_t) + payload.size() > ZX_CHANNEL_MAX_MSG_BYTES,
                  "attempted to write small message using large message writer");

    Bytes large_message_info = aligned_large_message_info(payload.size());
    HandleInfos handle_infos = BuildHandleInfos();
    ZX_ASSERT_MSG(handle_infos.size() <= 63,
                  "can only send a maximum of 63 harness-defined handles");

    handle_infos.push_back(kExpectedOverflowBufferHandleInfo);
    if (out_expected != nullptr) {
      *out_expected = Expected{
          .handle_infos = handle_infos,
          .channel_bytes = Bytes({header, aligned_large_message_info(payload.size())}),
          .vmo_bytes = payload,
      };
    }

    Bytes channel_bytes_in =
        Bytes({header, aligned_large_message_info(payload.size() + populate_unset_handles.size())});
    Bytes vmo_bytes_in = Bytes({populate_unset_handles, payload});
    ASSERT_OK(
        client.write_with_overflow(channel_bytes_in, vmo_bytes_in, BuildHandleDispositions()));
  }

  std::array<ByteVectorSize, kHandleCarryingElementsCount> byte_vector_sizes;
  std::array<HandlePresence, kHandleCarryingElementsCount> handles;
};

}  // namespace

// ////////////////////////////////////////////////////////////////////////
// Good decode tests
// ////////////////////////////////////////////////////////////////////////

void GoodDecodeSmallStructOfByteVector(ServerTest* testing, uint64_t method_ordinal) {
  uint32_t n = kSmallStructByteVectorSize;
  Bytes bytes_in = {
      header(kOneWayTxid, method_ordinal, fidl::MessageDynamicFlags::kStrictMethod),
      vector_header(n),
      repeat(kSomeByte).times(n),
  };

  ASSERT_OK(testing->client_end().write(bytes_in));
  WAIT_UNTIL_EXT(testing, [&]() { return testing->reporter().received_strict_one_way(); });
}

void GoodDecodeLargeStructOfByteVector(ServerTest* testing, uint64_t method_ordinal) {
  uint32_t n = kLargeStructByteVectorSize;
  uint32_t pad = FIDL_ALIGNMENT - (kLargeStructByteVectorSize % FIDL_ALIGNMENT);
  Bytes channel_bytes_in = {
      header(kOneWayTxid, method_ordinal, kStrictMethodAndByteOverflow),
      aligned_large_message_info(n + pad + kVectorHeaderSize),
  };
  Bytes vmo_bytes_in = {
      vector_header(n),
      repeat(kSomeByte).times(n),
      padding(pad),
  };

  ASSERT_OK(testing->client_end().write_with_overflow(channel_bytes_in, vmo_bytes_in));
  WAIT_UNTIL_EXT(testing, [&]() { return testing->reporter().received_strict_one_way(); });
}

void GoodDecodeSmallUnionOfByteVector(ServerTest* testing, uint64_t method_ordinal) {
  uint32_t n = kSmallUnionByteVectorSize;
  Bytes bytes_in = {
      header(kOneWayTxid, method_ordinal, fidl::MessageDynamicFlags::kStrictMethod),
      union_ordinal(1),
      out_of_line_envelope(n + kVectorHeaderSize, 0),
      vector_header(n),
      repeat(kSomeByte).times(n),
  };

  ASSERT_OK(testing->client_end().write(bytes_in));
  WAIT_UNTIL_EXT(testing, [&]() { return testing->reporter().received_strict_one_way(); });
}

void GoodDecodeLargeUnionOfByteVector(ServerTest* testing, uint64_t method_ordinal,
                                      fidl_xunion_tag_t xunion_ordinal) {
  uint32_t n = kLargeUnionByteVectorSize;
  uint32_t pad = FIDL_ALIGNMENT - (kLargeUnionByteVectorSize % FIDL_ALIGNMENT);
  Bytes channel_bytes_in = {
      header(kOneWayTxid, method_ordinal, kStrictMethodAndByteOverflow),
      aligned_large_message_info(n + pad + kUnionOrdinalAndEnvelopeSize + kVectorHeaderSize),
  };
  Bytes vmo_bytes_in = {
      union_ordinal(xunion_ordinal),
      out_of_line_envelope(n + pad + kVectorHeaderSize, 0),
      vector_header(n),
      repeat(kSomeByte).times(n),
      padding(pad),
  };

  ASSERT_OK(testing->client_end().write_with_overflow(channel_bytes_in, vmo_bytes_in));
  WAIT_UNTIL_EXT(testing, [&]() { return testing->reporter().received_strict_one_way(); });
}

LARGE_MESSAGE_SERVER_TEST(GoodDecodeBoundedKnownSmallMessage) {
  GoodDecodeSmallStructOfByteVector(this, kDecodeBoundedKnownToBeSmall);
}

LARGE_MESSAGE_SERVER_TEST(GoodDecodeBoundedMaybeSmallMessage) {
  GoodDecodeSmallStructOfByteVector(this, kDecodeBoundedMaybeLarge);
}

LARGE_MESSAGE_SERVER_TEST(GoodDecodeBoundedMaybeLargeMessage) {
  GoodDecodeLargeStructOfByteVector(this, kDecodeBoundedMaybeLarge);
}

LARGE_MESSAGE_SERVER_TEST(GoodDecodeSemiBoundedUnknowableSmallMessage) {
  GoodDecodeSmallUnionOfByteVector(this, kDecodeSemiBoundedBelievedToBeSmall);
}

LARGE_MESSAGE_SERVER_TEST(GoodDecodeSemiBoundedUnknowableLargeMessage) {
  GoodDecodeLargeUnionOfByteVector(this, kDecodeSemiBoundedBelievedToBeSmall, 2);
}

LARGE_MESSAGE_SERVER_TEST(GoodDecodeSemiBoundedMaybeSmallMessage) {
  GoodDecodeSmallUnionOfByteVector(this, kDecodeSemiBoundedMaybeLarge);
}

LARGE_MESSAGE_SERVER_TEST(GoodDecodeSemiBoundedMaybeLargeMessage) {
  GoodDecodeLargeUnionOfByteVector(this, kDecodeSemiBoundedMaybeLarge, 1);
}

LARGE_MESSAGE_SERVER_TEST(GoodDecodeUnboundedSmallMessage) {
  GoodDecodeSmallStructOfByteVector(this, kDecodeUnboundedMaybeLargeValue);
}

LARGE_MESSAGE_SERVER_TEST(GoodDecodeUnboundedLargeMessage) {
  GoodDecodeLargeStructOfByteVector(this, kDecodeUnboundedMaybeLargeValue);
}

LARGE_MESSAGE_SERVER_TEST(GoodDecode64HandleSmallMessage) {
  auto writer = UnboundedMaybeLargeResourceWriter::SmallMessageAnd64Handles();
  writer.WriteSmallMessageForDecode(client_end(),
                                    header(kOneWayTxid, kDecodeUnboundedMaybeLargeResource,
                                           fidl::MessageDynamicFlags::kStrictMethod));

  WAIT_UNTIL([this]() { return reporter().received_strict_one_way(); });
}

LARGE_MESSAGE_SERVER_TEST(GoodDecode63HandleLargeMessage) {
  auto writer = UnboundedMaybeLargeResourceWriter::LargeMessageAnd63Handles();
  writer.WriteLargeMessageForDecode(
      client_end(),
      header(kOneWayTxid, kDecodeUnboundedMaybeLargeResource, kStrictMethodAndByteOverflow));

  WAIT_UNTIL([this]() { return reporter().received_strict_one_way(); });
}

LARGE_MESSAGE_SERVER_TEST(GoodDecodeUnknownSmallMessage) {
  uint32_t n = kSmallStructByteVectorSize;
  Bytes bytes_in = {
      header(kOneWayTxid, kOrdinalFakeUnknownMethod, fidl::MessageDynamicFlags::kFlexibleMethod),
      vector_header(n),
      repeat(kSomeByte).times(n),
  };

  ASSERT_OK(client_end().write(bytes_in));
  WAIT_UNTIL([this]() { return reporter().received_unknown_method().has_value(); });
}

LARGE_MESSAGE_SERVER_TEST(GoodDecodeUnknownLargeMessage) {
  uint32_t n = kLargeStructByteVectorSize;
  uint32_t pad = FIDL_ALIGNMENT - (kLargeStructByteVectorSize % FIDL_ALIGNMENT);
  Bytes channel_bytes_in = {
      header(kOneWayTxid, kOrdinalFakeUnknownMethod, kFlexibleMethodAndByteOverflow),
      aligned_large_message_info(n + pad + kUnionOrdinalAndEnvelopeSize + kVectorHeaderSize),
  };
  Bytes vmo_bytes_in = {
      vector_header(n),
      repeat(kSomeByte).times(n),
      padding(pad),
  };

  ASSERT_OK(client_end().write_with_overflow(channel_bytes_in, vmo_bytes_in));
  WAIT_UNTIL([this]() { return reporter().received_unknown_method().has_value(); });
}

// ////////////////////////////////////////////////////////////////////////
// Good encode tests
// ////////////////////////////////////////////////////////////////////////

void GoodEncodeSmallStructOfByteVector(ServerTest* testing, uint64_t method_ordinal) {
  uint32_t n = kSmallStructByteVectorSize;
  Bytes bytes_in = {
      header(kTwoWayTxid, method_ordinal, fidl::MessageDynamicFlags::kStrictMethod),
      vector_header(n),
      repeat(kSomeByte).times(n),
  };
  Bytes bytes_out(bytes_in);

  ASSERT_OK(testing->client_end().write(bytes_in));
  ASSERT_OK(testing->client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(testing->client_end().read_and_check(bytes_out));
}

void GoodEncodeLargeStructOfByteVector(ServerTest* testing, uint64_t method_ordinal) {
  uint32_t n = kLargeStructByteVectorSize;
  uint32_t pad = FIDL_ALIGNMENT - (kLargeStructByteVectorSize % FIDL_ALIGNMENT);
  Bytes channel_bytes_in = {
      header(kTwoWayTxid, method_ordinal, kStrictMethodAndByteOverflow),
      aligned_large_message_info(n + pad + kVectorHeaderSize),
  };
  Bytes vmo_bytes_in = {
      vector_header(n),
      repeat(kSomeByte).times(n),
      padding(pad),
  };
  Bytes channel_bytes_out(channel_bytes_in);
  Bytes vmo_bytes_out(vmo_bytes_in);

  ASSERT_OK(testing->client_end().write_with_overflow(channel_bytes_in, vmo_bytes_in));
  ASSERT_OK(testing->client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(testing->client_end().read_and_check_with_overflow(
      channel_bytes_out, vmo_bytes_out, {kExpectedOverflowBufferHandleInfo}));
}

void GoodEncodeSmallUnionOfByteVector(ServerTest* testing, uint64_t method_ordinal) {
  uint32_t n = kSmallUnionByteVectorSize;
  Bytes bytes_in = {
      header(kTwoWayTxid, method_ordinal, fidl::MessageDynamicFlags::kStrictMethod),
      union_ordinal(1),
      out_of_line_envelope(n + kVectorHeaderSize, 0),
      vector_header(n),
      repeat(kSomeByte).times(n),
  };
  Bytes bytes_out(bytes_in);

  ASSERT_OK(testing->client_end().write(bytes_in));
  ASSERT_OK(testing->client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(testing->client_end().read_and_check(bytes_out));
}

void GoodEncodeLargeUnionOfByteVector(ServerTest* testing, uint64_t method_ordinal) {
  uint32_t n = kLargeUnionByteVectorSize;
  uint32_t pad = FIDL_ALIGNMENT - (kLargeUnionByteVectorSize % FIDL_ALIGNMENT);
  Bytes channel_bytes_in = {
      header(kTwoWayTxid, method_ordinal, kStrictMethodAndByteOverflow),
      aligned_large_message_info(n + pad + kUnionOrdinalAndEnvelopeSize + kVectorHeaderSize),
  };
  Bytes vmo_bytes_in = {
      union_ordinal(1), out_of_line_envelope(n + pad + kVectorHeaderSize, 0),
      vector_header(n), repeat(kSomeByte).times(n),
      padding(pad),
  };
  Bytes channel_bytes_out(channel_bytes_in);
  Bytes vmo_bytes_out(vmo_bytes_in);

  ASSERT_OK(testing->client_end().write_with_overflow(channel_bytes_in, vmo_bytes_in));
  ASSERT_OK(testing->client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(testing->client_end().read_and_check_with_overflow(
      channel_bytes_out, vmo_bytes_out, {kExpectedOverflowBufferHandleInfo}));
}

LARGE_MESSAGE_SERVER_TEST(GoodEncodeBoundedKnownSmallMessage) {
  GoodEncodeSmallStructOfByteVector(this, kEncodeBoundedKnownToBeSmall);
}

LARGE_MESSAGE_SERVER_TEST(GoodEncodeBoundedMaybeSmallMessage) {
  GoodEncodeSmallStructOfByteVector(this, kEncodeBoundedMaybeLarge);
}

LARGE_MESSAGE_SERVER_TEST(GoodEncodeBoundedMaybeLargeMessage) {
  GoodEncodeLargeStructOfByteVector(this, kEncodeBoundedMaybeLarge);
}

LARGE_MESSAGE_SERVER_TEST(GoodEncodeSemiBoundedKnownSmallMessage) {
  GoodEncodeSmallUnionOfByteVector(this, kEncodeSemiBoundedBelievedToBeSmall);
}

LARGE_MESSAGE_SERVER_TEST(GoodEncodeSemiBoundedMaybeSmallMessage) {
  GoodEncodeSmallUnionOfByteVector(this, kEncodeSemiBoundedMaybeLarge);
}

LARGE_MESSAGE_SERVER_TEST(GoodEncodeSemiBoundedMaybeLargeMessage) {
  GoodEncodeLargeUnionOfByteVector(this, kEncodeSemiBoundedMaybeLarge);
}

LARGE_MESSAGE_SERVER_TEST(GoodEncodeUnboundedSmallMessage) {
  GoodEncodeSmallStructOfByteVector(this, kEncodeUnboundedMaybeLargeValue);
}

LARGE_MESSAGE_SERVER_TEST(GoodEncodeUnboundedLargeMessage) {
  GoodEncodeLargeStructOfByteVector(this, kEncodeUnboundedMaybeLargeValue);
}

LARGE_MESSAGE_SERVER_TEST(GoodEncode64HandleSmallMessage) {
  auto writer = UnboundedMaybeLargeResourceWriter::SmallMessageAnd64Handles();
  Expected expect;
  writer.WriteSmallMessageForEncode(client_end(),
                                    header(kTwoWayTxid, kEncodeUnboundedMaybeLargeResource,
                                           fidl::MessageDynamicFlags::kStrictMethod),
                                    populate_unset_handles_false(), &expect);

  ASSERT_FALSE(expect.vmo_bytes.has_value());
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(client_end().read_and_check(expect.channel_bytes, expect.handle_infos));
}

LARGE_MESSAGE_SERVER_TEST(GoodEncode63HandleLargeMessage) {
  auto writer = UnboundedMaybeLargeResourceWriter::LargeMessageAnd63Handles();
  Expected expect;
  writer.WriteLargeMessageForEncode(
      client_end(),
      header(kTwoWayTxid, kEncodeUnboundedMaybeLargeResource, kStrictMethodAndByteOverflow),
      populate_unset_handles_false(), &expect);

  ASSERT_TRUE(expect.vmo_bytes.has_value());
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_READABLE));
  ASSERT_OK(client_end().read_and_check_with_overflow(
      expect.channel_bytes, expect.vmo_bytes.value(), expect.handle_infos));
}

// ////////////////////////////////////////////////////////////////////////
// Bad encode tests
// ////////////////////////////////////////////////////////////////////////

LARGE_MESSAGE_SERVER_TEST(BadEncode64HandleLargeMessage) {
  auto writer = UnboundedMaybeLargeResourceWriter::LargeMessageAnd0Handles();
  Expected expect;
  writer.WriteLargeMessageForEncode(
      client_end(),
      header(kTwoWayTxid, kEncodeUnboundedMaybeLargeResource, kStrictMethodAndByteOverflow),
      populate_unset_handles_true(), &expect);

  ASSERT_TRUE(expect.vmo_bytes.has_value());
  WAIT_UNTIL([this]() { return reporter().reply_encoding_failed().has_value(); });
  ASSERT_EQ(EncodingFailureKind::kLargeMessage64Handles,
            reporter().reply_encoding_failed()->kind());
  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

// TODO(fxbug.dev/114259): Write remaining tests for encoding/decoding large messages.

}  // namespace server_suite
