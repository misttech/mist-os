// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package cpp

import (
	"bytes"
	"encoding/hex"
	"fmt"
	"strconv"
	"strings"

	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/lib/cpp"
	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/lib/hlcpp"
	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/lib/ir"
	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/lib/mixer"
	"go.fuchsia.dev/fuchsia/tools/fidl/lib/fidlgen"
)

func buildEqualityCheck(actualExpr string, expectedValue ir.Value, decl mixer.Declaration, handleKoidVectorName string) string {
	builder := equalityCheckBuilder{
		handleKoidVectorName: handleKoidVectorName,
	}
	builder.visit(actualExpr, expectedValue, decl)
	return builder.String()
}

// Generator of new variable names from a sequence.
type varSeq int

func (v *varSeq) next() string {
	*v++
	return fmt.Sprintf("f%d", *v)
}

type equalityCheckBuilder struct {
	strings.Builder
	varSeq varSeq
	// Name of a C++ variable containing an vector of zx_koid_t of handle values
	// This is read-only and is used for checking handle koid equality.
	handleKoidVectorName string
}

func (b *equalityCheckBuilder) write(format string, vals ...interface{}) {
	b.WriteString(fmt.Sprintf(format, vals...))
}

func (b *equalityCheckBuilder) createAndAssignVar(val string) string {
	varName := b.varSeq.next()
	b.write("[[maybe_unused]] const auto& %s = %s;\n", varName, val)
	return varName
}

func (b *equalityCheckBuilder) construct(typename string, fmtStr string, args ...interface{}) string {
	return fmt.Sprintf("%s(%s)", typename, fmt.Sprintf(fmtStr, args...))
}

func (b *equalityCheckBuilder) assertEquals(actual, expected string) {
	b.write("ASSERT_EQ(%s, %s);\n", actual, expected)
}
func (b *equalityCheckBuilder) assertStringEquals(actual, expected string) {
	b.write("ASSERT_STREQ(%s, %s);\n", actual, expected)
}
func (b *equalityCheckBuilder) assertNotEquals(actual, expected string) {
	b.write("ASSERT_NE(%s, %s);\n", actual, expected)
}
func (b *equalityCheckBuilder) assertFalse(value string) {
	b.write("ASSERT_FALSE(%s);\n", value)
}
func (b *equalityCheckBuilder) assertTrue(value string) {
	b.write("ASSERT_TRUE(%s);\n", value)
}
func (b *equalityCheckBuilder) assertNull(value string) {
	b.write("ASSERT_NULL(%s);\n", value)
}
func (b *equalityCheckBuilder) assertAbsent(value string) {
	b.write("ASSERT_FALSE(%s.has_value());\n", value)
}
func (b *equalityCheckBuilder) assertPresent(value string) {
	b.write("ASSERT_TRUE(%s.has_value());\n", value)
}

func (b *equalityCheckBuilder) visit(actualExpr string, expectedValue ir.Value, decl mixer.Declaration) {
	switch expectedValue := expectedValue.(type) {
	case bool:
		b.assertEquals(actualExpr, b.construct(cpp.TypeName(decl), "%t", expectedValue))
		return
	case int64, uint64, float64:
		switch decl := decl.(type) {
		case mixer.PrimitiveDeclaration, *mixer.EnumDecl:
			b.assertEquals(actualExpr, b.construct(cpp.TypeName(decl), cpp.FormatPrimitive(expectedValue)))
			return
		case *mixer.BitsDecl:
			b.assertEquals(actualExpr, fmt.Sprintf("static_cast<%s>(%s)", cpp.DeclName(decl), cpp.FormatPrimitive(expectedValue)))
			return
		}
	case ir.RawFloat:
		switch decl.(*mixer.FloatDecl).Subtype() {
		case fidlgen.Float32:
			b.assertEquals(actualExpr, fmt.Sprintf("([] { uint32_t u = %#b; float f; memcpy(&f, &u, sizeof(float)); return f; })()", expectedValue))
			return
		case fidlgen.Float64:
			b.assertEquals(actualExpr, fmt.Sprintf("([] { uint64_t u = %#b; double d; memcpy(&d, &u, sizeof(double)); return d; })()", expectedValue))
			return
		}
	case string:
		dereferencedActual := actualExpr
		if decl.IsNullable() {
			b.assertTrue(fmt.Sprintf("%s.has_value()", actualExpr))
			dereferencedActual = fmt.Sprintf("(*%s)", actualExpr)
		}
		b.assertStringEquals(dereferencedActual, escapeStr(expectedValue))
		return
	case ir.AnyHandle:
		switch decl := decl.(type) {
		case *mixer.HandleDecl:
			b.visitHandle(actualExpr, expectedValue, decl, ownedHandle)
			return
		case *mixer.ClientEndDecl:
			b.visitClientEnd(actualExpr, expectedValue, decl)
			return
		case *mixer.ServerEndDecl:
			b.visitServerEnd(actualExpr, expectedValue, decl)
			return
		}
	case ir.Record:
		switch decl := decl.(type) {
		case *mixer.StructDecl:
			b.visitStruct(actualExpr, expectedValue, decl)
			return
		case *mixer.TableDecl:
			b.visitTable(actualExpr, expectedValue, decl)
			return
		case *mixer.UnionDecl:
			b.visitUnion(actualExpr, expectedValue, decl)
			return
		}
	case []ir.Value:
		b.visitList(actualExpr, expectedValue, decl.(mixer.ListDeclaration))
		return
	case nil:
		switch decl.(type) {
		case *mixer.StringDecl:
			b.assertFalse(fmt.Sprintf("%s.has_value()", actualExpr))
			return
		case *mixer.HandleDecl:
			b.assertFalse(fmt.Sprintf("%s.is_valid()", actualExpr))
			return
		case *mixer.UnionDecl:
			b.assertAbsent(actualExpr)
			return
		case *mixer.VectorDecl:
			b.assertFalse(fmt.Sprintf("%s.has_value()", actualExpr))
			return
		case *mixer.StructDecl:
			b.assertAbsent(actualExpr)
			return
		}
	}
	panic(fmt.Sprintf("not implemented: %T (decl: %T)", expectedValue, decl))
}

type handleOwnership int64

const (
	unownedHandle handleOwnership = iota
	ownedHandle
)

func (b *equalityCheckBuilder) visitHandle(actualExpr string, expectedValue ir.AnyHandle, decl *mixer.HandleDecl, ownership handleOwnership) {
	actualVar := b.createAndAssignVar(actualExpr)
	resultVar := b.varSeq.next()
	var handleValueExpr string
	switch ownership {
	case unownedHandle:
		handleValueExpr = fmt.Sprintf("%s->get()", actualVar)
	case ownedHandle:
		handleValueExpr = fmt.Sprintf("%s.get()", actualVar)
	}
	b.write(`
	zx_info_handle_basic_t %[1]s_info;
	ASSERT_OK(zx_object_get_info(%[2]s, ZX_INFO_HANDLE_BASIC, &%[1]s_info, sizeof(%[1]s_info), nullptr, nullptr));
	ASSERT_EQ(%[1]s_info.koid, %[3]s[%[4]d]);
	`, resultVar, handleValueExpr, b.handleKoidVectorName, expectedValue.GetHandle())
	if h, ok := expectedValue.(ir.RestrictedHandle); ok {
		b.write("ASSERT_EQ(%s_info.type, %d);", resultVar, h.Type)
		b.write("ASSERT_EQ(%s_info.rights, %d);", resultVar, h.Rights)
	}
}

func (b *equalityCheckBuilder) visitClientEnd(actualExpr string, expectedValue ir.AnyHandle, decl *mixer.ClientEndDecl) {
	b.visitHandle(fmt.Sprintf("(%s).handle()", actualExpr), expectedValue, decl.UnderlyingHandleDecl(), unownedHandle)
}

func (b *equalityCheckBuilder) visitServerEnd(actualExpr string, expectedValue ir.AnyHandle, decl *mixer.ServerEndDecl) {
	b.visitHandle(fmt.Sprintf("(%s).handle()", actualExpr), expectedValue, decl.UnderlyingHandleDecl(), unownedHandle)
}

func (b *equalityCheckBuilder) visitStruct(actualExpr string, expectedValue ir.Record, decl *mixer.StructDecl) {
	actualVar := b.createAndAssignVar(actualExpr)
	op := "."
	if decl.IsNullable() {
		op = "->"
		b.assertPresent(actualVar)
	}
	for _, field := range expectedValue.Fields {
		actualFieldExpr := fmt.Sprintf("%s%s%s()", actualVar, op, field.Key.Name)
		b.visit(actualFieldExpr, field.Value, decl.Field(field.Key.Name))
	}
}

func (b *equalityCheckBuilder) visitTable(actualExpr string, expectedValue ir.Record, decl *mixer.TableDecl) {
	actualVar := b.createAndAssignVar(actualExpr)
	expectedFieldValues := map[string]ir.Value{}
	for _, field := range expectedValue.Fields {
		if field.Key.IsUnknown() {
			// Unknowns table members are dropped when decoding in the new C++ bindings.
			// There are also no way to access them.
			continue
		}
		expectedFieldValues[field.Key.Name] = field.Value
	}
	for _, fieldName := range decl.FieldNames() {
		if expectedFieldValue, ok := expectedFieldValues[fieldName]; ok {
			b.assertTrue(fmt.Sprintf("%s.%s().has_value()", actualVar, fieldName))
			actualFieldExpr := fmt.Sprintf("%s.%s().value()", actualVar, fieldName)
			b.visit(actualFieldExpr, expectedFieldValue, decl.Field(fieldName))
		} else {
			b.assertFalse(fmt.Sprintf("%s.%s().has_value()", actualVar, fieldName))
		}
	}
}

func (b *equalityCheckBuilder) visitUnion(actualExpr string, expectedValue ir.Record, decl *mixer.UnionDecl) {
	actualVar := b.createAndAssignVar(actualExpr)
	op := "."
	if decl.IsNullable() {
		op = "->"
		b.assertPresent(actualVar)
	}
	if len(expectedValue.Fields) != 1 {
		panic("shouldn't happen")
	}
	field := expectedValue.Fields[0]
	if field.Key.IsUnknown() {
		// The natural types discards all information except the fact that
		// the member was unknown. So there's nothing else to check.
		b.assertTrue(fmt.Sprintf("%s%sIsUnknown()", actualVar, op))
		return
	}
	b.assertEquals(
		fmt.Sprintf("%s%sWhich()", actualVar, op),
		fmt.Sprintf("%s::Tag::k%s", cpp.DeclName(decl), fidlgen.ToUpperCamelCase(field.Key.Name)))
	b.assertTrue(fmt.Sprintf("%s%s%s().has_value()", actualVar, op, fidlgen.ToSnakeCase(field.Key.Name)))
	actualFieldExpr := fmt.Sprintf("%s%s%s().value()", actualVar, op, fidlgen.ToSnakeCase(field.Key.Name))
	b.visit(actualFieldExpr, field.Value, decl.Field(field.Key.Name))
}

func (b *equalityCheckBuilder) visitList(actualExpr string, expectedValue []ir.Value, decl mixer.ListDeclaration) {
	var actualVar string
	if decl.IsNullable() {
		actualVar = b.createAndAssignVar(actualExpr + ".value()")
	} else {
		actualVar = b.createAndAssignVar(actualExpr)
	}
	if _, ok := decl.(*mixer.VectorDecl); ok {
		b.assertEquals(fmt.Sprintf("%s.size()", actualVar), fmt.Sprintf("%d", len(expectedValue)))
	}
	for i, item := range expectedValue {
		lhs := fmt.Sprintf("%s[%d]", actualVar, i)
		switch item.(type) {
		case bool:
			// prevents `error: no viable conversion from '__bit_iterator<std::vector<bool>, false>' to 'const void *'`
			lhs = fmt.Sprintf("bool(%s)", lhs)
		}
		b.visit(lhs, item, decl.Elem())
	}
}

func (b *equalityCheckBuilder) visitUnknownBytes(actualExpr string, expectedValue []byte) {
	b.write(`
	std::vector<uint8_t> bytes%[1]s = %[2]s;
	ASSERT_EQ(bytes%[1]s, %[3]s);
	`,
		b.varSeq.next(), hlcpp.BuildBytes(expectedValue), actualExpr)
}

func (b *equalityCheckBuilder) visitUnknownHandles(actualExpr string, expectedValue []ir.Handle) {
	b.write(`
	std::vector<zx_handle_t> handles%[1]s = %[2]s;
	ASSERT_EQ(handles%[1]s.size(), %[3]s.size());
	for (uint32_t i = 0; i < handles%[1]s.size(); ++i) {
		zx_handle_t actual = handles%[1]s[i];
		zx_info_handle_basic_t %[1]s_info_actual;
		ASSERT_OK(zx_object_get_info(actual, ZX_INFO_HANDLE_BASIC, &%[1]s_info_actual, sizeof(%[1]s_info_actual), nullptr, nullptr));

		zx_handle_t expected = %[3]s[i].get();
		zx_info_handle_basic_t %[1]s_info_expected;
		ASSERT_OK(zx_object_get_info(expected, ZX_INFO_HANDLE_BASIC, &%[1]s_info_expected, sizeof(%[1]s_info_expected), nullptr, nullptr));

		ASSERT_EQ(%[1]s_info_expected.koid, %[1]s_info_actual.koid);
	}
	`, b.varSeq.next(), hlcpp.BuildRawHandlesFromHandleInfos(expectedValue), actualExpr)
}

func escapeStr(value string) string {
	if fidlgen.PrintableASCII(value) {
		return strconv.Quote(value)
	}
	var (
		buf    bytes.Buffer
		src    = []byte(value)
		dstLen = hex.EncodedLen(len(src))
		dst    = make([]byte, dstLen)
	)
	hex.Encode(dst, src)
	buf.WriteRune('"')
	for i := 0; i < dstLen; i += 2 {
		buf.WriteString("\\x")
		buf.WriteByte(dst[i])
		buf.WriteByte(dst[i+1])
	}
	buf.WriteRune('"')
	return buf.String()
}
