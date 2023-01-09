// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

import 'dart:typed_data';

import 'package:meta/meta.dart';
import 'package:zircon/zircon.dart';

import 'codec.dart';
import 'error.dart';
import 'struct.dart';
import 'types.dart';
import 'wire_format.dart';
// ignore_for_file: public_member_api_docs

const int kMessageHeaderSize = 16;
const int kLargeMessageInfoSize = 16;
const int kMessageTxidOffset = 0;
const int kMessageFlagOffset = 4;
const int kMessageDynamicFlagOffset = 6;
const int kMessageMagicOffset = 7;
const int kMessageOrdinalOffset = 8;

const int kMagicNumberInitial = 1;
const int kWireFormatV2FlagMask = 2;

const int _kDynamicFlagsFlexible = 0x80;
const int _kDynamicFlagsByteOverflow = 0x40;

enum CallStrictness {
  strict,
  flexible,
}

enum CallOverflowing {
  large,
  small,
}

/// Convert CallStrictness to a byte that can be inserted into the dynamic flags
/// portion of a message.
int strictnessToFlags(CallStrictness strictness) {
  switch (strictness) {
    case CallStrictness.strict:
      return 0x00;
    case CallStrictness.flexible:
      return _kDynamicFlagsFlexible;
  }
}

/// Extract the CallStrictness from the dynamic flags byte of the message
/// header.
CallStrictness strictnessFromFlags(int dynamicFlags) {
  if ((dynamicFlags & _kDynamicFlagsFlexible) != 0) {
    return CallStrictness.flexible;
  }
  return CallStrictness.strict;
}

// TODO(fxbug.dev/114263): Add `overflowingToFlags`.

/// Extract the CallOverflowing from the dynamic flags byte of the message
/// header.
CallOverflowing overflowingFromFlags(int dynamicFlags) {
  if ((dynamicFlags & _kDynamicFlagsByteOverflow) != 0) {
    return CallOverflowing.large;
  }
  return CallOverflowing.small;
}

class _BaseMessage {
  _BaseMessage(this.data);

  final ByteData data;

  int get txid => data.getUint32(kMessageTxidOffset, Endian.little);
  int get ordinal => data.getUint64(kMessageOrdinalOffset, Endian.little);
  int get magic => data.getUint8(kMessageMagicOffset);
  WireFormat parseWireFormat() {
    if ((data.getUint8(kMessageFlagOffset) & kWireFormatV2FlagMask) != 0) {
      return WireFormat.v2;
    }
    throw FidlError(
        'unknown wire format', FidlErrorCode.fidlUnsupportedWireFormat);
  }

  CallStrictness get strictness =>
      strictnessFromFlags(data.getUint8(kMessageDynamicFlagOffset));

  CallOverflowing get overflowing =>
      overflowingFromFlags(data.getUint8(kMessageDynamicFlagOffset));

  bool isCompatible() => magic == kMagicNumberInitial;

  void hexDump() {
    const int width = 16;
    Uint8List list = Uint8List.view(data.buffer, 0);
    StringBuffer buffer = StringBuffer();
    final RegExp isPrintable = RegExp(r'\w');
    for (int i = 0; i < data.lengthInBytes; i += width) {
      StringBuffer hex = StringBuffer();
      StringBuffer printable = StringBuffer();
      for (int j = 0; j < width && i + j < data.lengthInBytes; j++) {
        int v = list[i + j];
        String s = v.toRadixString(16);
        if (s.length == 1)
          hex.write('0$s ');
        else
          hex.write('$s ');

        s = String.fromCharCode(v);
        if (isPrintable.hasMatch(s)) {
          printable.write(s);
        } else {
          printable.write('.');
        }
      }
      buffer.write('${hex.toString().padRight(3 * width)} $printable\n');
    }

    print('==================================================\n'
        '$buffer'
        '==================================================');
  }
}

class IncomingMessage extends _BaseMessage {
  IncomingMessage(data, this.handleInfos) : super(data);
  IncomingMessage.fromReadEtcResult(ReadEtcResult result)
      : assert(result.status == ZX.OK),
        handleInfos = result.handleInfos,
        super(result.bytes);
  @visibleForTesting
  IncomingMessage.fromOutgoingMessage(OutgoingMessage outgoingMessage)
      : handleInfos = outgoingMessage.handleDispositions
            .map((handleDisposition) => HandleInfo(handleDisposition.handle,
                handleDisposition.type, handleDisposition.rights))
            .toList(),
        super(outgoingMessage.data);

  final List<HandleInfo> handleInfos;

  void closeHandles() {
    for (int i = 0; i < handleInfos.length; ++i) {
      handleInfos[i].handle.close();
    }
  }

  @override
  String toString() {
    return 'IncomingMessage(numBytes=${data.lengthInBytes}, numHandles=${handleInfos.length})';
  }
}

class OutgoingMessage extends _BaseMessage {
  OutgoingMessage(data, this.handleDispositions) : super(data);

  set txid(int value) =>
      data.setUint32(kMessageTxidOffset, value, Endian.little);

  final List<HandleDisposition> handleDispositions;

  void closeHandles() {
    for (int i = 0; i < handleDispositions.length; ++i) {
      handleDispositions[i].handle.close();
    }
  }

  @override
  String toString() {
    return 'OutgoingMessage(numBytes=${data.lengthInBytes}, numHandles=${handleDispositions.length})';
  }
}

/// Encodes a FIDL message that contains a single parameter.
void encodeMessage<T>(
    Encoder encoder, int inlineSize, MemberType typ, T value) {
  encoder.alloc(inlineSize, 0);
  typ.encode(encoder, value, kMessageHeaderSize, 1);
}

/// Encodes a FIDL message with multiple parameters.  The callback parameter
/// provides a decoder that is initialized on the provided Message, which
/// callers can use to decode specific types.  This functionality (encoding
/// multiple parameters) is implemented using a callback because each call to
/// MemberType.encode() must pass in a concrete type, rather than an element
/// popped from a List<FidlType>.
void encodeMessageWithCallback(Encoder encoder, int inlineSize, Function() f) {
  encoder.alloc(inlineSize, 0);
  f();
}

void _validateDecoding(Decoder decoder) {
  // The ordering of the following two checks is important: if there is both unclaimed memory and
  // unclaimed handles, we should do the unclaimed handles clean up first (namely, closing all open)
  // handles.
  if (decoder.countUnclaimedHandles() > 0) {
    // If there are unclaimed handles at the end of the decoding, close all
    // handles to the best of our ability, and throw an error.
    for (var handleInfo in decoder.handleInfos) {
      try {
        handleInfo.handle.close();
        // ignore: avoid_catches_without_on_clauses
      } catch (e) {
        // best effort
      }
    }

    int unclaimed = decoder.countUnclaimedHandles();
    int total = decoder.handleInfos.length;
    throw FidlError(
        'Message contains extra handles (unclaimed: $unclaimed, total: $total)',
        FidlErrorCode.fidlTooManyHandles);
  }

  if (decoder.countUnclaimedBytes() > 0) {
    int unclaimed = decoder.countUnclaimedBytes();
    int total = decoder.data.lengthInBytes;
    throw FidlError(
        'Message contains unread bytes (unclaimed: $unclaimed, total: $total)',
        FidlErrorCode.fidlTooManyBytes);
  }
}

/// Decodes a FIDL message that contains a single parameter.
T decodeMessage<T>(IncomingMessage message, int inlineSize, MemberType typ) {
  return decodeMessageWithCallback(message, inlineSize,
      (Decoder decoder, int offset) {
    return typ.decode(decoder, offset, 1);
  });
}

/// Decodes a (possibly large) FIDL message that contains a single parameter.
T decodeMaybeLargeMessage<T>(
    IncomingMessage message, int inlineSize, MemberType typ) {
  return decodeMaybeLargeMessageWithCallback(message, inlineSize,
      (Decoder decoder, int offset) {
    return typ.decode(decoder, offset, 1);
  });
}

/// Decodes a FIDL message with multiple parameters.  The callback parameter
/// provides a decoder that is initialized on the provided Message, which
/// callers can use to decode specific types.  The return result of the callback
/// (e.g. the decoded parameters, wrapped in a containing class/struct) is
/// returned as the result of the function.  This functionality (decoding
/// multiple parameters) is implemented using a callback because returning a
/// list would be insufficient: the list would be of type List<FidlType>,
/// whereas we want to retain concrete types of each decoded parameter.  The
/// only way to accomplish this in Dart is to pass in a function that collects
/// these multiple values into a bespoke, properly typed class.
T decodeMessageWithCallback<T>(
    IncomingMessage message, int inlineSize, DecodeMessageCallback<T> f) {
  final int size = kMessageHeaderSize + inlineSize;
  final Decoder decoder = Decoder(message)..claimBytes(size, 0);
  T out = f(decoder, kMessageHeaderSize);
  final int padding = align(size) - size;
  decoder.checkPadding(size, padding);
  _validateDecoding(decoder);
  return out;
}

/// This is a special-case of [Struct], which, unlike other codegen-created instances, does not have
/// a static [_structDecode] method, as this type is only every meant to be decoded as a top-level
/// payload body.
class LargeMessageInfo extends Struct {
  const LargeMessageInfo({
    required this.flags,
    required this.reserved,
    required this.msgByteCount,
  });

  final int flags;
  final int reserved;
  final int msgByteCount;

  @override
  List<Object?> get $fields {
    return <Object?>[
      flags,
      reserved,
      msgByteCount,
    ];
  }

  static const $fieldType0 = Uint32Type();
  static const $fieldType1 = Uint32Type();
  static const $fieldType2 = Uint64Type();

  // TODO(fxbug.dev/114263): currently untested/unimplemented encode logic.
  @override
  void $encode(Encoder $encoder, int $offset, int $depth) {
    $fieldType0.encode($encoder, flags, $offset + 0, $depth);
    $fieldType1.encode($encoder, reserved, $offset + 4, $depth);
    $fieldType2.encode($encoder, msgByteCount, $offset + 8, $depth);
  }
}

const kLargeMessageInfoFlagsType =
    MemberType<int>(type: Uint32Type(), offset: 0);
const kLargeMessageInfoReservedType =
    MemberType<int>(type: Uint32Type(), offset: 4);
const kLargeMessageInfoMsgByteCountType =
    MemberType<int>(type: Uint64Type(), offset: 8);

T decodeMaybeLargeMessageWithCallback<T>(
    IncomingMessage message, int inlineSize, DecodeMessageCallback<T> f) {
  if (message.overflowing == CallOverflowing.small) {
    return decodeMessageWithCallback<T>(message, inlineSize, f);
  }

  // Perform a bit of surgery - remove the handles array attached to the original
  // [IncomingMessage], and create a new [IncomingMessage] with only the original's bytes. This
  // will be used to decode the outer [LargeMessageInfo] struct, which we can then use to properly
  // read the VMO containing the remainder of the data.
  IncomingMessage handlesStrippedMessage = IncomingMessage(message.data, []);
  LargeMessageInfo largeMessageInfo =
      decodeMessageWithCallback<LargeMessageInfo>(
          handlesStrippedMessage, kLargeMessageInfoSize,
          (Decoder decoder, int offset) {
    return LargeMessageInfo(
        flags: kLargeMessageInfoFlagsType.decode(decoder, offset, 1),
        reserved: kLargeMessageInfoReservedType.decode(decoder, offset, 1),
        msgByteCount:
            kLargeMessageInfoMsgByteCountType.decode(decoder, offset, 1));
  });

  // Read the overflow bytes from the overflow buffer containing VMO, then combine those bytes
  // with the handles from the original message to make the arguments passed to the decoder
  // callback.
  SizedVmo overflowVmo =
      SizedVmo(message.handleInfos.last.handle, largeMessageInfo.msgByteCount);
  ReadResult result = overflowVmo.read(largeMessageInfo.msgByteCount);

  // Decode the body bytes only - no need to redo the header.
  final Decoder decoder = Decoder.fromRawArgs(
      ByteData.view(
          result.bytes.buffer, result.bytes.offsetInBytes, result.numBytes),
      message.handleInfos.sublist(0, message.handleInfos.length - 1),
      WireFormat.v2)
    ..claimBytes(inlineSize, 0);
  T out = f(decoder, 0);
  final int padding = align(inlineSize) - inlineSize;
  decoder.checkPadding(inlineSize, padding);
  _validateDecoding(decoder);
  return out;
}

typedef DecodeMessageCallback<T> = T Function(Decoder decoder, int offset);
typedef IncomingMessageSink = void Function(IncomingMessage message);
typedef OutgoingMessageSink = void Function(OutgoingMessage message);
