// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package dart

import (
	"fmt"
	"strconv"
	"strings"

	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/lib/ir"
	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/lib/mixer"
	"go.fuchsia.dev/fuchsia/tools/fidl/lib/fidlgen"
)

func buildHandleDefs(defs []ir.HandleDef) string {
	if len(defs) == 0 {
		return ""
	}
	var builder strings.Builder
	builder.WriteString("[\n")
	for i, d := range defs {
		builder.WriteString("HandleDef(")
		switch d.Subtype {
		case fidlgen.HandleSubtypeEvent:
			builder.WriteString(fmt.Sprint("HandleSubtype.event, "))
		case fidlgen.HandleSubtypeChannel:
			builder.WriteString(fmt.Sprint("HandleSubtype.channel, "))
		default:
			panic(fmt.Sprintf("unknown handle subtype: %v", d.Subtype))
		}
		// Write indices corresponding to the .gidl file handle_defs block.
		builder.WriteString(fmt.Sprintf("%d),// #%d\n", d.Rights, i))
	}
	builder.WriteString("]")
	return builder.String()
}

func buildHandleValues(handles []ir.Handle) string {
	var builder strings.Builder
	builder.WriteString("[\n")
	for _, h := range handles {
		builder.WriteString(fmt.Sprintf("%s,", buildHandleValue(h)))
	}
	builder.WriteString("]")
	return builder.String()
}

func buildUnknownTableData(fields []ir.Field) string {
	if len(fields) == 0 {
		return "null"
	}
	var builder strings.Builder
	builder.WriteString("{\n")
	for _, field := range fields {
		unknownData := field.Value.(ir.UnknownData)
		builder.WriteString(fmt.Sprintf(
			"%d: fidl.UnknownRawData(\n%s,\n%s\n),",
			field.Key.UnknownOrdinal,
			buildBytes(unknownData.Bytes),
			buildHandleValues(unknownData.Handles)))
	}
	builder.WriteString("}")
	return builder.String()
}

func visit(value ir.Value, decl mixer.Declaration) string {
	switch value := value.(type) {
	case bool:
		return strconv.FormatBool(value)
	case int64, uint64, float64:
		switch decl := decl.(type) {
		case *mixer.IntegerDecl, *mixer.FloatDecl:
			return fmt.Sprintf("%#v", value)
		case mixer.NamedDeclaration:
			return fmt.Sprintf("%s.ctor(%#v)", typeName(decl), value)
		}
	case ir.RawFloat:
		switch decl.(*mixer.FloatDecl).Subtype() {
		case fidlgen.Float32:
			return fmt.Sprintf("Uint32List.fromList([%#x]).buffer.asByteData().getFloat32(0, Endian.host)", value)
		case fidlgen.Float64:
			return fmt.Sprintf("Uint64List.fromList([%#x]).buffer.asByteData().getFloat64(0, Endian.host)", value)
		}
	case string:
		return toDartStr(value)
	case ir.AnyHandle:
		rawHandle := buildHandleValue(value.GetHandle())
		handleDecl := decl.(*mixer.HandleDecl)
		switch handleDecl.Subtype() {
		case fidlgen.HandleSubtypeNone:
			return rawHandle
		case fidlgen.HandleSubtypeChannel:
			return fmt.Sprintf("Channel(%s)", rawHandle)
		case fidlgen.HandleSubtypeEvent:
			// Dart does not support events, so events are mapped to bare handles
			return rawHandle
		default:
			panic(fmt.Sprintf("unknown handle subtype: %v", handleDecl.Subtype()))
		}
	case ir.Record:
		switch decl := decl.(type) {
		case *mixer.StructDecl:
			return onRecord(value, decl)
		case *mixer.TableDecl:
			return onRecord(value, decl)
		case *mixer.UnionDecl:
			return onUnion(value, decl)
		}
	case []ir.Value:
		switch decl := decl.(type) {
		case *mixer.ArrayDecl:
			return onList(value, decl)
		case *mixer.VectorDecl:
			return onList(value, decl)
		}
	case nil:
		if !decl.IsNullable() {
			if _, ok := decl.(*mixer.HandleDecl); ok {
				return "Handle.invalid()"
			}
			panic(fmt.Sprintf("got nil for non-nullable type: %T", decl))
		}
		return "null"
	}
	panic(fmt.Sprintf("not implemented: %T", value))
}

func onRecord(value ir.Record, decl mixer.RecordDeclaration) string {
	var args []string
	var unknownTableFields []ir.Field
	for _, field := range value.Fields {
		if field.Key.IsUnknown() {
			unknownTableFields = append(unknownTableFields, field)
			continue
		}
		val := visit(field.Value, decl.Field(field.Key.Name))
		args = append(args, fmt.Sprintf("%s: %s", fidlgen.ToLowerCamelCase(field.Key.Name), val))
	}
	if len(unknownTableFields) > 0 {
		args = append(args,
			fmt.Sprintf("$unknownData: %s", buildUnknownTableData(unknownTableFields)))
	}
	return fmt.Sprintf("%s(%s)", fidlgen.ToUpperCamelCase(value.Name), strings.Join(args, ", "))
}

func onUnion(value ir.Record, decl *mixer.UnionDecl) string {
	for _, field := range value.Fields {
		if field.Key.IsUnknown() {
			unknownData := field.Value.(ir.UnknownData)
			return fmt.Sprintf(
				"%s.with$UnknownData(%d, fidl.UnknownRawData(%s, %s))",
				value.Name,
				field.Key.UnknownOrdinal,
				buildBytes(unknownData.Bytes),
				buildHandleValues(unknownData.Handles))
		}
		val := visit(field.Value, decl.Field(field.Key.Name))
		return fmt.Sprintf("%s.with%s(%s)", value.Name, fidlgen.ToUpperCamelCase(field.Key.Name), val)
	}
	// Not currently possible to construct a union in dart with an invalid value.
	panic("unions must have a value set")
}

func onList(value []ir.Value, decl mixer.ListDeclaration) string {
	var elements []string
	elemDecl := decl.Elem()
	for _, item := range value {
		elements = append(elements, visit(item, elemDecl))
	}
	if integerDecl, ok := elemDecl.(*mixer.IntegerDecl); ok {
		typeName := fidlgen.ToUpperCamelCase(string(integerDecl.Subtype()))
		return fmt.Sprintf("%sList.fromList([%s])", typeName, strings.Join(elements, ", "))
	}
	if floatDecl, ok := elemDecl.(*mixer.FloatDecl); ok {
		typeName := fidlgen.ToUpperCamelCase(string(floatDecl.Subtype()))
		return fmt.Sprintf("%sList.fromList([%s])", typeName, strings.Join(elements, ", "))
	}
	return fmt.Sprintf("[%s]", strings.Join(elements, ", "))
}

func typeName(decl mixer.NamedDeclaration) string {
	parts := strings.Split(decl.Name(), "/")
	lastPart := parts[len(parts)-1]
	return dartTypeName(lastPart)
}

func buildHandleValue(handle ir.Handle) string {
	return fmt.Sprintf("handles[%d]", handle)
}
