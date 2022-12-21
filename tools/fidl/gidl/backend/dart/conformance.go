// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package dart

import (
	"bytes"
	_ "embed"
	"fmt"
	"strings"
	"text/template"

	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/lib/config"
	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/lib/ir"
	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/lib/mixer"
	"go.fuchsia.dev/fuchsia/tools/fidl/lib/fidlgen"
)

var (
	//go:embed conformance.tmpl
	conformanceTmplText string

	conformanceTmpl = template.Must(template.New("conformanceTmpl").Parse(conformanceTmplText))
)

type tmplInput struct {
	EncodeSuccessCases []encodeSuccessCase
	DecodeSuccessCases []decodeSuccessCase
	EncodeFailureCases []encodeFailureCase
	DecodeFailureCases []decodeFailureCase
}

type encodeSuccessCase struct {
	EncoderName, Name, Value, ValueType, Bytes, HandleDefs, Handles string
}

type decodeSuccessCase struct {
	Name, WireFormat, Value, ValueType, Bytes, HandleDefs, Handles, UnusedHandles string
}

type encodeFailureCase struct {
	EncoderName, Name, Value, ValueType, ErrorCode, HandleDefs string
}

type decodeFailureCase struct {
	Name, WireFormat, ValueType, Bytes, ErrorCode, HandleDefs, Handles string
}

// Generate generates dart tests.
func GenerateConformanceTests(gidl ir.All, fidl fidlgen.Root, config config.GeneratorConfig) ([]byte, error) {
	schema := mixer.BuildSchema(fidl)
	encodeSuccessCases, err := encodeSuccessCases(gidl.EncodeSuccess, schema)
	if err != nil {
		return nil, err
	}
	decodeSuccessCases, err := decodeSuccessCases(gidl.DecodeSuccess, schema)
	if err != nil {
		return nil, err
	}
	encodeFailureCases, err := encodeFailureCases(gidl.EncodeFailure, schema)
	if err != nil {
		return nil, err
	}
	decodeFailureCases, err := decodeFailureCases(gidl.DecodeFailure, schema)
	if err != nil {
		return nil, err
	}
	var buf bytes.Buffer
	err = conformanceTmpl.Execute(&buf, tmplInput{
		EncodeSuccessCases: encodeSuccessCases,
		DecodeSuccessCases: decodeSuccessCases,
		EncodeFailureCases: encodeFailureCases,
		DecodeFailureCases: decodeFailureCases,
	})
	return buf.Bytes(), err
}

func encodeSuccessCases(gidlEncodeSuccesses []ir.EncodeSuccess, schema mixer.Schema) ([]encodeSuccessCase, error) {
	var encodeSuccessCases []encodeSuccessCase
	for _, encodeSuccess := range gidlEncodeSuccesses {
		decl, err := schema.ExtractDeclarationEncodeSuccess(encodeSuccess.Value, encodeSuccess.HandleDefs)
		if err != nil {
			return nil, fmt.Errorf("encode success %s: %s", encodeSuccess.Name, err)
		}
		valueStr := visit(encodeSuccess.Value, decl)
		valueType := typeName(decl)
		for _, encoding := range encodeSuccess.Encodings {
			if !wireFormatSupported(encoding.WireFormat) {
				continue
			}
			encodeSuccessCases = append(encodeSuccessCases, encodeSuccessCase{
				EncoderName: encoderName(encoding.WireFormat),
				Name:        testCaseName(encodeSuccess.Name, encoding.WireFormat),
				Value:       valueStr,
				ValueType:   valueType,
				Bytes:       buildBytes(encoding.Bytes),
				HandleDefs:  buildHandleDefs(encodeSuccess.HandleDefs),
				Handles:     toDartIntList(ir.GetHandlesFromHandleDispositions(encoding.HandleDispositions)),
			})
		}
	}
	return encodeSuccessCases, nil
}

func decodeSuccessCases(gidlDecodeSuccesses []ir.DecodeSuccess, schema mixer.Schema) ([]decodeSuccessCase, error) {
	var decodeSuccessCases []decodeSuccessCase
	for _, decodeSuccess := range gidlDecodeSuccesses {
		decl, err := schema.ExtractDeclaration(decodeSuccess.Value, decodeSuccess.HandleDefs)
		if err != nil {
			return nil, fmt.Errorf("decode success %s: %s", decodeSuccess.Name, err)
		}
		valueStr := visit(decodeSuccess.Value, decl)
		valueType := typeName(decl)
		for _, encoding := range decodeSuccess.Encodings {
			if !wireFormatSupported(encoding.WireFormat) {
				continue
			}
			decodeSuccessCases = append(decodeSuccessCases, decodeSuccessCase{
				Name:       testCaseName(decodeSuccess.Name, encoding.WireFormat),
				WireFormat: wireFormatName(encoding.WireFormat),
				Value:      valueStr,
				ValueType:  valueType,
				Bytes:      buildBytes(encoding.Bytes),
				HandleDefs: buildHandleDefs(decodeSuccess.HandleDefs),
				Handles:    toDartIntList(encoding.Handles),
				UnusedHandles: toDartIntList(ir.GetUnusedHandles(decodeSuccess.Value,
					encoding.Handles)),
			})
		}
	}
	return decodeSuccessCases, nil
}

func encodeFailureCases(gidlEncodeFailures []ir.EncodeFailure, schema mixer.Schema) ([]encodeFailureCase, error) {
	var encodeFailureCases []encodeFailureCase
	for _, encodeFailure := range gidlEncodeFailures {
		decl, err := schema.ExtractDeclarationUnsafe(encodeFailure.Value)
		if err != nil {
			return nil, fmt.Errorf("encode failure %s: %s", encodeFailure.Name, err)
		}
		errorCode, err := dartErrorCode(encodeFailure.Err)
		if err != nil {
			return nil, fmt.Errorf("encode failure %s: %s", encodeFailure.Name, err)
		}
		valueStr := visit(encodeFailure.Value, decl)
		valueType := typeName(decl)
		for _, wireFormat := range supportedWireFormats {
			encodeFailureCases = append(encodeFailureCases, encodeFailureCase{
				EncoderName: encoderName(wireFormat),
				Name:        testCaseName(encodeFailure.Name, wireFormat),
				Value:       valueStr,
				ValueType:   valueType,
				ErrorCode:   errorCode,
				HandleDefs:  buildHandleDefs(encodeFailure.HandleDefs),
			})
		}
	}
	return encodeFailureCases, nil
}

func decodeFailureCases(gidlDecodeFailures []ir.DecodeFailure, schema mixer.Schema) ([]decodeFailureCase, error) {
	var decodeFailureCases []decodeFailureCase
	for _, decodeFailure := range gidlDecodeFailures {
		_, err := schema.ExtractDeclarationByName(decodeFailure.Type)
		if err != nil {
			return nil, fmt.Errorf("decode failure %s: %s", decodeFailure.Name, err)
		}
		errorCode, err := dartErrorCode(decodeFailure.Err)
		if err != nil {
			return nil, fmt.Errorf("decode failure %s: %s", decodeFailure.Name, err)
		}
		valueType := dartTypeName(decodeFailure.Type)
		for _, encoding := range decodeFailure.Encodings {
			if !wireFormatSupported(encoding.WireFormat) {
				continue
			}
			decodeFailureCases = append(decodeFailureCases, decodeFailureCase{
				Name:       testCaseName(decodeFailure.Name, encoding.WireFormat),
				WireFormat: wireFormatName(encoding.WireFormat),
				ValueType:  valueType,
				Bytes:      buildBytes(encoding.Bytes),
				ErrorCode:  errorCode,
				HandleDefs: buildHandleDefs(decodeFailure.HandleDefs),
				Handles:    toDartIntList(encoding.Handles),
			})
		}
	}
	return decodeFailureCases, nil
}

var supportedWireFormats = []ir.WireFormat{
	ir.V2WireFormat,
}

func wireFormatSupported(wireFormat ir.WireFormat) bool {
	for _, wf := range supportedWireFormats {
		if wireFormat == wf {
			return true
		}
	}
	return false
}

func testCaseName(baseName string, wireFormat ir.WireFormat) string {
	return fidlgen.SingleQuote(fmt.Sprintf("%s_%s", baseName, wireFormat))
}

func encoderName(wireFormat ir.WireFormat) string {
	return fmt.Sprintf("Encoders.%s", wireFormat)
}

func wireFormatName(wireFormat ir.WireFormat) string {
	return fmt.Sprintf("fidl.WireFormat.%s", wireFormat)
}

func dartTypeName(inputType string) string {
	return fmt.Sprintf("k%s_Type", inputType)
}

func buildBytes(bytes []byte) string {
	var builder strings.Builder
	builder.WriteString("Uint8List.fromList([\n")
	for i, b := range bytes {
		builder.WriteString(fmt.Sprintf("0x%02x,", b))
		if i%8 == 7 {
			// Note: empty comments are written to preserve formatting. See:
			// https://github.com/dart-lang/dart_style/wiki/FAQ#why-does-the-formatter-mess-up-my-collection-literals
			builder.WriteString(" //\n")
		}
	}
	builder.WriteString("])")
	return builder.String()
}

func toDartStr(value string) string {
	var buf bytes.Buffer
	buf.WriteRune('\'')
	for _, r := range value {
		if 0x20 <= r && r <= 0x7e { // printable ASCII rune
			buf.WriteRune(r)
		} else {
			buf.WriteString(fmt.Sprintf(`\u{%x}`, r))
		}
	}
	buf.WriteRune('\'')
	return buf.String()
}

func toDartIntList(handles []ir.Handle) string {
	var builder strings.Builder
	builder.WriteString("[\n")
	for i, handle := range handles {
		builder.WriteString(fmt.Sprintf("%d,", handle))
		if i%8 == 7 {
			// Note: empty comments are written to preserve formatting. See:
			// https://github.com/dart-lang/dart_style/wiki/FAQ#why-does-the-formatter-mess-up-my-collection-literals
			builder.WriteString(" //\n")
		}
	}
	builder.WriteString("]")
	return builder.String()
}

// Dart error codes are defined in sdk/dart/fidl/lib/src/error.dart.
var dartErrorCodeNames = map[ir.ErrorCode]string{
	ir.CountExceedsLimit:                  "fidlCountExceedsLimit",
	ir.EnvelopeBytesExceedMessageLength:   "unknown",
	ir.EnvelopeHandlesExceedMessageLength: "unknown",
	ir.ExceededMaxOutOfLineDepth:          "fidlExceededMaxOutOfLineDepth",
	ir.IncorrectHandleType:                "fidlIncorrectHandleType",
	ir.InvalidBoolean:                     "fidlInvalidBoolean",
	ir.InvalidEmptyStruct:                 "fidlInvalidPaddingByte",
	ir.InvalidInlineBitInEnvelope:         "fidlInvalidInlineBitInEnvelope",
	ir.InvalidInlineMarkerInEnvelope:      "fidlInvalidInlineMarkerInEnvelope",
	ir.InvalidNumBytesInEnvelope:          "fidlInvalidNumBytesInEnvelope",
	ir.InvalidNumHandlesInEnvelope:        "fidlInvalidNumHandlesInEnvelope",
	ir.InvalidPaddingByte:                 "fidlInvalidPaddingByte",
	ir.InvalidPresenceIndicator:           "fidlInvalidPresenceIndicator",
	ir.MissingRequiredHandleRights:        "fidlMissingRequiredHandleRights",
	ir.NonEmptyStringWithNullBody:         "fidlNonEmptyStringWithNullBody",
	ir.NonEmptyVectorWithNullBody:         "fidlNonEmptyVectorWithNullBody",
	ir.NonNullableTypeWithNullValue:       "fidlNonNullableTypeWithNullValue",
	ir.NonResourceUnknownHandles:          "fidlNonResourceHandle",
	ir.StrictBitsUnknownBit:               "fidlInvalidBit",
	ir.StrictEnumUnknownValue:             "fidlInvalidEnumValue",
	ir.StrictUnionUnknownField:            "fidlStrictUnionUnknownField",
	ir.StringCountExceeds32BitLimit:       "fidlStringTooLong",
	ir.StringNotUtf8:                      "unknown",
	ir.StringTooLong:                      "fidlStringTooLong",
	ir.TableCountExceeds32BitLimit:        "fidlCountExceedsLimit",
	ir.TooFewBytes:                        "fidlTooFewBytes",
	ir.TooFewBytesInPrimaryObject:         "fidlTooFewBytes",
	ir.TooFewHandles:                      "fidlTooFewHandles",
	ir.TooManyBytesInMessage:              "fidlTooManyBytes",
	ir.TooManyHandlesInMessage:            "fidlTooManyHandles",
	ir.UnionFieldNotSet:                   "unknown",
	ir.UnexpectedOrdinal:                  "fidlCountExceedsLimit",
	ir.VectorCountExceeds32BitLimit:       "fidlCountExceedsLimit",
}

func dartErrorCode(code ir.ErrorCode) (string, error) {
	if str, ok := dartErrorCodeNames[code]; ok {
		return fmt.Sprintf("fidl.FidlErrorCode.%s", str), nil
	}
	return "", fmt.Errorf("no dart error string defined for error code %s", code)
}
