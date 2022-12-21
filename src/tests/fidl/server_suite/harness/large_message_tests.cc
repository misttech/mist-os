// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fidl/fidl.serversuite/cpp/natural_types.h"
#include "lib/fidl/cpp/unified_messaging.h"
#include "lib/fit/function.h"
#include "src/tests/fidl/server_suite/harness/harness.h"
#include "src/tests/fidl/server_suite/harness/ordinals.h"

using namespace channel_util;
using namespace fidl_serversuite;

namespace server_suite {

namespace {

const uint32_t kVectorHeaderSize = 16;
const uint32_t kVmoPageSize = 4096;
const uint32_t kUnionOrdinalAndEnvelopeSize = 16;
const uint32_t kDefaultElementsCount = kHandleCarryingElementsCount - 1;
const zx_rights_t kExpectedOverflowBufferRights = kOverflowBufferRights;
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

struct OutgoingLargeMessage {
  Bytes channel_bytes;
  Bytes vmo_bytes;
  HandleDispositions handles;
};

// A pair of types that pass in a large message before it is about to be sent, allowing its encoded
// bytes to be "corrupted" in arbitrary ways.
struct CorruptibleLargeMessage {
  Bytes header;
  Bytes large_message_info;
  Bytes payload;
  HandleDispositions handles;
};
using MessageCorrupter = fit::function<void(CorruptibleLargeMessage*)>;

// Because |UnboundedMaybeLargeResource| is used so widely, and needs to have many parts (handles,
// VMO-stored data, etc) assembled just so in a variety of configurations (small/large with 0, 63,
// or 64 handles, plus all manner of mis-encodings), this helper struct keeps track of all of the
// bookkeeping necessary when building an |UnboundedMaybeLargeResource| of a certain shape.
class UnboundedMaybeLargeResourceWriter {
 public:
  UnboundedMaybeLargeResourceWriter(UnboundedMaybeLargeResourceWriter&&) = default;
  UnboundedMaybeLargeResourceWriter& operator=(UnboundedMaybeLargeResourceWriter&&) = default;
  UnboundedMaybeLargeResourceWriter(const UnboundedMaybeLargeResourceWriter&) = delete;
  UnboundedMaybeLargeResourceWriter& operator=(const UnboundedMaybeLargeResourceWriter&) = delete;

  using ByteVectorSize = size_t;
  enum class HandlePresence {
    kAbsent = 0,
    kPresent = 1,
  };

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

  // The last |corrupter| argument allows us to mess with the message bytes, to test |BadDecode*|
  // cases. The default argument is a no-op corrupter - that is, one that does not corrupt a message
  // at all. Useful to test decoding of "good" messages, while also ensuring that the corrupter
  // works as expected (rather than always producing bad data, thereby not actually testing all of
  // the "bad" decode cases).
  void WriteLargeMessageForDecode(
      Channel& client, Bytes header, MessageCorrupter corrupter = [](CorruptibleLargeMessage*) {}) {
    WriteCorruptibleLargeMessage(client, header, std::move(corrupter));
  }

  void WriteLargeMessageForEncode(Channel& client, Bytes header, Bytes populate_unset_handles,
                                  Expected* out_expected) {
    WriteLargeMessage(client, header, populate_unset_handles, out_expected);
  }

  // Identical to |WriteLargeMessageForDecode()|, except it returns the about-to-be-sent message
  // instead of calling |.write_with_overflow()| for you. Useful for when we want to alter (corrupt)
  // the fully encoded and ready to be written data before sending over the |client_end|. When using
  // this method, the overflow buffer VMO must be built and added to the |handles| list manually
  // before sending.
  OutgoingLargeMessage PrepareLargeMessage(
      Bytes header, MessageCorrupter corrupter = [](CorruptibleLargeMessage*) {}) {
    Bytes payload = BuildPayload();
    ZX_ASSERT_MSG(sizeof(fidl_message_header_t) + payload.size() > ZX_CHANNEL_MAX_MSG_BYTES,
                  "attempted to write small message using large message writer");

    CorruptibleLargeMessage corruptible = {
        .header = std::move(header),
        .large_message_info = aligned_large_message_info(payload.size()),
        .payload = std::move(payload),
        .handles = BuildHandleDispositions(),
    };
    ZX_ASSERT_MSG(corruptible.handles.size() <= 63,
                  "can only send a maximum of 63 harness-defined handles");

    corrupter(&corruptible);
    return {
        .channel_bytes =
            Bytes({std::move(corruptible.header), std::move(corruptible.large_message_info)}),
        .vmo_bytes = std::move(corruptible.payload),
        .handles = std::move(corruptible.handles),
    };
  }

 private:
  Bytes BuildPayload() {
    std::vector<Bytes> payload_bytes;
    // Add the inline portions of each |element| entry.
    for (size_t i = 0; i < kHandleCarryingElementsCount; i++) {
      payload_bytes.push_back(
          {handles[i] == HandlePresence::kPresent ? handle_present() : handle_absent(), padding(4),
           vector_header(byte_vector_sizes[i])});
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
            .operation = ZX_HANDLE_OP_MOVE,
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

  void WriteLargeMessage(Channel& client, Bytes header, Bytes populate_unset_handles,
                         Expected* out_expected) {
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

  // Provides a lambda to alter the message bytes in specific way in the service of |BadDecode*|
  // tests. The way this works: first we create all of the pieces of a "correct" large message for
  // this instance, then we pass those to the lambda, allowing one or more of those components to be
  // corrupted in some interesting way before we send them off.
  //
  // This method must only be when sending messages using the `DecodeUnboundedMaybeLargeResource`
  // FIDL method - `EncodeUnboundedMaybeLargeResource` has an extra field, and must be built using
  // `WriteLargeMessage` instead.
  void WriteCorruptibleLargeMessage(Channel& client, Bytes header, MessageCorrupter corrupter) {
    OutgoingLargeMessage out = PrepareLargeMessage(std::move(header), std::move(corrupter));
    ASSERT_OK(client.write_with_overflow(std::move(out.channel_bytes), std::move(out.vmo_bytes),
                                         std::move(out.handles)));
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
// Bad decode tests
// ////////////////////////////////////////////////////////////////////////

LARGE_MESSAGE_SERVER_TEST(BadDecodeByteOverflowFlagSetOnSmallMessage) {
  auto writer = UnboundedMaybeLargeResourceWriter::SmallMessageAnd64Handles();

  // The `kStrictMethodAndByteOverflow` flag here is incorrect - there is no overflow buffer.
  writer.WriteSmallMessageForDecode(
      client_end(),
      header(kOneWayTxid, kDecodeUnboundedMaybeLargeResource, kStrictMethodAndByteOverflow));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

LARGE_MESSAGE_SERVER_TEST(BadDecodeByteOverflowFlagUnsetOnLargeMessage) {
  auto writer = UnboundedMaybeLargeResourceWriter::LargeMessageAnd63Handles();

  // The `kStrictMethod` flag here is incorrect - the `byte_overflow` flag should be set too.
  writer.WriteLargeMessageForDecode(client_end(),
                                    header(kOneWayTxid, kDecodeUnboundedMaybeLargeResource,
                                           fidl::MessageDynamicFlags::kStrictMethod));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

LARGE_MESSAGE_SERVER_TEST(BadDecodeLargeMessageInfoOmitted) {
  auto writer = UnboundedMaybeLargeResourceWriter::LargeMessageAnd63Handles();

  // Corrupter lambda removes entire `large_message_info`.
  writer.WriteLargeMessageForDecode(
      client_end(),
      header(kOneWayTxid, kDecodeUnboundedMaybeLargeResource, kStrictMethodAndByteOverflow),
      [](CorruptibleLargeMessage* corruptible) { corruptible->large_message_info = Bytes(); });

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

LARGE_MESSAGE_SERVER_TEST(BadDecodeLargeMessageInfoTooSmall) {
  auto writer = UnboundedMaybeLargeResourceWriter::LargeMessageAnd63Handles();

  // Corrupter lambda removes top half of `large_message_info`.
  writer.WriteLargeMessageForDecode(
      client_end(),
      header(kOneWayTxid, kDecodeUnboundedMaybeLargeResource, kStrictMethodAndByteOverflow),
      [](CorruptibleLargeMessage* corruptible) {
        auto large_message_info_bytes = corruptible->large_message_info.as_vec();
        corruptible->large_message_info =
            Bytes(large_message_info_bytes.begin() + 8, large_message_info_bytes.end());
      });

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

LARGE_MESSAGE_SERVER_TEST(BadDecodeLargeMessageInfoTooLarge) {
  auto writer = UnboundedMaybeLargeResourceWriter::LargeMessageAnd63Handles();

  // Corrupter lambda adds trailing zeroes to `large_message_info`.
  writer.WriteLargeMessageForDecode(
      client_end(),
      header(kOneWayTxid, kDecodeUnboundedMaybeLargeResource, kStrictMethodAndByteOverflow),
      [](CorruptibleLargeMessage* corruptible) {
        corruptible->large_message_info = Bytes({corruptible->large_message_info, u64(0)});
      });

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

LARGE_MESSAGE_SERVER_TEST(BadDecodeLargeMessageInfoTopHalfUnzeroed) {
  auto writer = UnboundedMaybeLargeResourceWriter::LargeMessageAnd63Handles();

  // Corrupter lambda swaps in non-zero data at the first byte of `large_message_info`.
  writer.WriteLargeMessageForDecode(
      client_end(),
      header(kOneWayTxid, kDecodeUnboundedMaybeLargeResource, kStrictMethodAndByteOverflow),
      [](CorruptibleLargeMessage* corruptible) { *(corruptible->large_message_info.data()) = 1; });

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

LARGE_MESSAGE_SERVER_TEST(BadDecodeLargeMessageInfoByteCountIsZero) {
  auto writer = UnboundedMaybeLargeResourceWriter::LargeMessageAnd63Handles();

  // Corrupter lambda sets the `msg_byte_count` of the `large_message_info` to 0, and truncates the
  // payload to match.
  writer.WriteLargeMessageForDecode(
      client_end(),
      header(kOneWayTxid, kDecodeUnboundedMaybeLargeResource, kStrictMethodAndByteOverflow),
      [](CorruptibleLargeMessage* corruptible) {
        corruptible->large_message_info = Bytes({u64(0), u64(0)});
        corruptible->payload = Bytes();
      });

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

LARGE_MESSAGE_SERVER_TEST(BadDecodeLargeMessageInfoByteCountBelowMinimum) {
  auto writer = UnboundedMaybeLargeResourceWriter::LargeMessageAnd63Handles();

  // Corrupter lambda sets the `msg_byte_count` of the `large_message_info` to 65520, the largest
  // possible "too small" value that is still 8-byte aligned. It also truncates the payload to
  // match.
  writer.WriteLargeMessageForDecode(
      client_end(),
      header(kOneWayTxid, kDecodeUnboundedMaybeLargeResource, kStrictMethodAndByteOverflow),
      [](CorruptibleLargeMessage* corruptible) {
        uint64_t truncated_size = ZX_CHANNEL_MAX_MSG_BYTES - sizeof(fidl_message_header_t);
        std::vector<uint8_t>& payload_data = corruptible->payload.as_vec();
        corruptible->large_message_info = Bytes({u64(0), u64(truncated_size)});
        corruptible->payload = Bytes(payload_data.begin(), payload_data.begin() + truncated_size);
      });

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

LARGE_MESSAGE_SERVER_TEST(BadDecodeLargeMessageInfoByteCountTooSmall) {
  auto writer = UnboundedMaybeLargeResourceWriter::LargeMessageAnd63Handles();

  // Corrupter lambda sets the `msg_byte_count` of the `large_message_info` to be 8 bytes too small.
  writer.WriteLargeMessageForDecode(
      client_end(),
      header(kOneWayTxid, kDecodeUnboundedMaybeLargeResource, kStrictMethodAndByteOverflow),
      [](CorruptibleLargeMessage* corruptible) {
        corruptible->large_message_info =
            Bytes({u64(0), u64(corruptible->payload.size() - FIDL_ALIGNMENT)});
      });

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

LARGE_MESSAGE_SERVER_TEST(BadDecodeLargeMessageInfoByteCountTooLarge) {
  auto writer = UnboundedMaybeLargeResourceWriter::LargeMessageAnd63Handles();

  // Corrupter lambda sets the `msg_byte_count` of the `large_message_info` to be 8 bytes too large.
  writer.WriteLargeMessageForDecode(
      client_end(),
      header(kOneWayTxid, kDecodeUnboundedMaybeLargeResource, kStrictMethodAndByteOverflow),
      [](CorruptibleLargeMessage* corruptible) {
        corruptible->large_message_info =
            Bytes({u64(0), u64(corruptible->payload.size() + FIDL_ALIGNMENT)});
      });

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

LARGE_MESSAGE_SERVER_TEST(BadDecodeLargeMessageNoHandles) {
  auto writer = UnboundedMaybeLargeResourceWriter::LargeMessageAnd0Handles();
  OutgoingLargeMessage out = writer.PrepareLargeMessage(
      header(kOneWayTxid, kDecodeUnboundedMaybeLargeResource, kStrictMethodAndByteOverflow));

  // We deliberately "forget" to attach the overflow buffer VMO's handle to the outgoing handle
  // list.
  ASSERT_OK(client_end().write_overflow_vmo(out.vmo_bytes, out.handles));
  client_end().write(out.channel_bytes, HandleDispositions());

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

LARGE_MESSAGE_SERVER_TEST(BadDecodeLargeMessageTooFewHandles) {
  auto writer = UnboundedMaybeLargeResourceWriter::LargeMessageAnd63Handles();
  OutgoingLargeMessage out = writer.PrepareLargeMessage(
      header(kOneWayTxid, kDecodeUnboundedMaybeLargeResource, kStrictMethodAndByteOverflow));

  // Simulate the overflow buffer handle overwriting the last handle, rather than appending to the
  // list.
  out.handles.pop_back();
  ASSERT_OK(client_end().write_overflow_vmo(out.vmo_bytes, out.handles));
  ASSERT_OK(client_end().write(out.channel_bytes, out.handles));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

LARGE_MESSAGE_SERVER_TEST(BadDecodeLargeMessage64Handles) {
  auto writer = UnboundedMaybeLargeResourceWriter::LargeMessageAnd63Handles();
  OutgoingLargeMessage out = writer.PrepareLargeMessage(
      header(kOneWayTxid, kDecodeUnboundedMaybeLargeResource, kStrictMethodAndByteOverflow));

  // Simulate 64 handles somehow getting attached to a large message by overwriting the overflow
  // buffer handle with a non-VMO handle.
  zx::event event;
  zx::event::create(0, &event);
  out.handles.push_back(zx_handle_disposition_t{
      .operation = ZX_HANDLE_OP_MOVE,
      .handle = event.release(),
      .type = ZX_OBJ_TYPE_EVENT,
      .rights = ZX_DEFAULT_EVENT_RIGHTS,
  });
  ASSERT_OK(client_end().write(out.channel_bytes, out.handles));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

LARGE_MESSAGE_SERVER_TEST(BadDecodeLargeMessageLastHandleNotVmo) {
  auto writer = UnboundedMaybeLargeResourceWriter::LargeMessageAnd63Handles();
  OutgoingLargeMessage out = writer.PrepareLargeMessage(
      header(kOneWayTxid, kDecodeUnboundedMaybeLargeResource, kStrictMethodAndByteOverflow));

  // Swap the last two handles, to simulate an invalid handle in the final position.
  ASSERT_OK(client_end().write_overflow_vmo(out.vmo_bytes, out.handles));
  zx_handle_disposition_t vmo_handle = out.handles.back();
  out.handles.pop_back();
  out.handles.insert(out.handles.end() - 1, vmo_handle);
  ASSERT_OK(client_end().write(out.channel_bytes, out.handles));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

LARGE_MESSAGE_SERVER_TEST(BadDecodeLargeMessageLastHandleInsufficientRights) {
  auto writer = UnboundedMaybeLargeResourceWriter::LargeMessageAnd63Handles();
  OutgoingLargeMessage out = writer.PrepareLargeMessage(
      header(kOneWayTxid, kDecodeUnboundedMaybeLargeResource, kStrictMethodAndByteOverflow));

  // Disabling the |ZX_RIGHT_INSPECT| is done on purpose, because this doesn't prevent functionality
  // of the normal course of large message exchange, but is still a required right that is useful in
  // other (debugging) contexts, and is per spec required.
  zx::vmo vmo;
  zx::vmo::create(out.vmo_bytes.size(), 0, &vmo);
  ASSERT_OK(vmo.write(out.vmo_bytes.data(), 0, out.vmo_bytes.size()));
  out.handles.push_back(zx_handle_disposition_t{
      .operation = ZX_HANDLE_OP_MOVE,
      .handle = vmo.release(),
      .type = ZX_OBJ_TYPE_VMO,
      .rights = kOverflowBufferRights & !ZX_RIGHT_INSPECT,
  });
  ASSERT_OK(client_end().write(out.channel_bytes, out.handles));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

LARGE_MESSAGE_SERVER_TEST(BadDecodeLargeMessageLastHandleExcessiveRights) {
  auto writer = UnboundedMaybeLargeResourceWriter::LargeMessageAnd63Handles();
  OutgoingLargeMessage out = writer.PrepareLargeMessage(
      header(kOneWayTxid, kDecodeUnboundedMaybeLargeResource, kStrictMethodAndByteOverflow));

  // Add the |ZX_RIGHT_WRITE| to corrupt the handle rights.
  zx::vmo vmo;
  zx::vmo::create(out.vmo_bytes.size(), 0, &vmo);
  ASSERT_OK(vmo.write(out.vmo_bytes.data(), 0, out.vmo_bytes.size()));
  out.handles.push_back(zx_handle_disposition_t{
      .operation = ZX_HANDLE_OP_MOVE,
      .handle = vmo.release(),
      .type = ZX_OBJ_TYPE_VMO,
      .rights = kOverflowBufferRights | ZX_RIGHT_WRITE,
  });
  ASSERT_OK(client_end().write(out.channel_bytes, out.handles));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
}

LARGE_MESSAGE_SERVER_TEST(BadDecodeLargeMessageVmoTooSmall) {
  auto writer = UnboundedMaybeLargeResourceWriter::LargeMessageAnd63Handles();
  OutgoingLargeMessage out = writer.PrepareLargeMessage(
      header(kOneWayTxid, kDecodeUnboundedMaybeLargeResource, kStrictMethodAndByteOverflow));

  // Make the VMO smaller than the `msg_byte_count` indicates. Because the underlying "size" of a
  // VMO is rounded up to the memory page size of the system (4096 bytes), to do this properly, we
  // must make it more than one memory page smaller. Otherwise, the "removed" memory may just get
  // zeroed, instead of fully truncated, as the VMO is expanded to fill the last page.
  out.vmo_bytes.as_vec().resize(out.vmo_bytes.size() - kVmoPageSize - FIDL_ALIGNMENT);
  ASSERT_OK(client_end().write_overflow_vmo(out.vmo_bytes, out.handles));
  ASSERT_OK(client_end().write(out.channel_bytes, out.handles));

  ASSERT_OK(client_end().wait_for_signal(ZX_CHANNEL_PEER_CLOSED));
  ASSERT_FALSE(client_end().is_signal_present(ZX_CHANNEL_READABLE));
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

}  // namespace server_suite
