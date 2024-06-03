// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package dynfidl

import (
	"bytes"
	_ "embed"
	"fmt"
	"strconv"
	"strings"
	"text/template"

	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/lib/config"
	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/lib/ir"
	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/lib/mixer"
	"go.fuchsia.dev/fuchsia/tools/fidl/gidl/lib/rust"
	"go.fuchsia.dev/fuchsia/tools/fidl/lib/fidlgen"
)

var (
	//go:embed conformance.tmpl
	conformanceTmplText string

	conformanceTmpl = template.Must(template.New("conformanceTmpl").Parse(conformanceTmplText))
)

type conformanceTmplInput struct {
	EncodeSuccessCases []encodeSuccessCase
}

type encodeSuccessCase struct {
	Name, Value, Bytes string
}

func GenerateConformanceTests(gidl ir.All, fidl fidlgen.Root, config config.GeneratorConfig) ([]byte, error) {
	schema := mixer.BuildSchema(fidl)

	// dynfidl only supports encode tests (it's an encoder)
	encodeSuccessCases, err := encodeSuccessCases(gidl.EncodeSuccess, schema)
	if err != nil {
		return nil, err
	}

	input := conformanceTmplInput{
		EncodeSuccessCases: encodeSuccessCases,
	}
	var buf bytes.Buffer
	err = conformanceTmpl.Execute(&buf, input)
	return buf.Bytes(), err
}

func encodeSuccessCases(gidlEncodeSuccesses []ir.EncodeSuccess, schema mixer.Schema) ([]encodeSuccessCase, error) {
	var encodeSuccessCases []encodeSuccessCase
	for _, encodeSuccess := range gidlEncodeSuccesses {
		decl, err := schema.ExtractDeclarationEncodeSuccess(encodeSuccess.Value, encodeSuccess.HandleDefs)
		if err != nil {
			return nil, fmt.Errorf("encode success %s: %s", encodeSuccess.Name, err)
		}
		if !isSupported(decl) {
			continue
		}
		visited := visit(encodeSuccess.Value, decl)
		for _, encoding := range encodeSuccess.Encodings {
			name := fidlgen.ToSnakeCase(fmt.Sprintf("%s_%s", encodeSuccess.Name, encoding.WireFormat))
			encodeSuccessCases = append(encodeSuccessCases, encodeSuccessCase{
				Name:  name,
				Value: visited.ValueStr,
				Bytes: rust.BuildBytes(encoding.Bytes),
			})
		}
	}
	return encodeSuccessCases, nil
}

// check whether dynfidl supports the layout specified by the decl
func isSupported(decl mixer.Declaration) bool {
	if decl.IsNullable() {
		return false
	}

	switch decl := decl.(type) {
	case *mixer.StructDecl:
		if decl.IsResourceType() {
			return false
		}
		for _, fieldName := range decl.FieldNames() {
			if !isSupportedStructField(decl.Field(fieldName)) {
				return false
			}
		}
		return true
	case *mixer.ArrayDecl, *mixer.BitsDecl, *mixer.BoolDecl, *mixer.EnumDecl,
		*mixer.FloatDecl, *mixer.HandleDecl, *mixer.IntegerDecl, *mixer.StringDecl,
		*mixer.TableDecl, *mixer.UnionDecl, *mixer.VectorDecl:
		return false
	default:
		panic(fmt.Sprintf("unrecognized type %s", decl))
	}
}

// check whether dynfidl supports the layout specified by the decl as the field of a struct
func isSupportedStructField(decl mixer.Declaration) bool {
	if decl.IsNullable() {
		return false
	}

	switch decl := decl.(type) {
	case *mixer.BoolDecl, *mixer.IntegerDecl, *mixer.StringDecl:
		return true
	case *mixer.StructDecl:
		if decl.IsResourceType() {
			return false
		}
		for _, fieldName := range decl.FieldNames() {
			if !isSupportedStructField(decl.Field(fieldName)) {
				return false
			}
		}
		return true
	case *mixer.VectorDecl:
		return isSupportedVectorElement(decl.Elem())
	case *mixer.ArrayDecl, *mixer.BitsDecl, *mixer.EnumDecl, *mixer.FloatDecl,
		*mixer.HandleDecl, *mixer.TableDecl, *mixer.UnionDecl:
		return false
	default:
		panic(fmt.Sprintf("unrecognized type %s", decl))
	}
}

// check whether dynfidl supports the layout specified by the decl as an element in a vector
func isSupportedVectorElement(decl mixer.Declaration) bool {
	if decl.IsNullable() {
		return false
	}

	switch decl := decl.(type) {
	case *mixer.BoolDecl, *mixer.IntegerDecl, *mixer.StringDecl:
		return true
	case *mixer.VectorDecl:
		// dynfidl only supports vectors-of-vectors-of-bytes
		switch decl := decl.Elem().(type) {
		case mixer.PrimitiveDeclaration:
			switch decl.Subtype() {
			case fidlgen.Uint8:
				return true
			default:
				return false
			}
		default:
			return false
		}
	case *mixer.ArrayDecl, *mixer.BitsDecl, *mixer.EnumDecl, *mixer.FloatDecl,
		*mixer.HandleDecl, *mixer.StructDecl, *mixer.TableDecl, *mixer.UnionDecl:
		return false
	default:
		panic(fmt.Sprintf("unrecognized type %s", decl))
	}
}

type visitResult struct {
	ValueStr            string
	OuterVariant        outerVariant
	InnerEnumAndVariant string
}

type outerVariant int

const (
	_ outerVariant = iota
	basicVariant
	vectorVariant
	structVariant
	unsupportedVariant
)

func (v outerVariant) String() string {
	switch v {
	case basicVariant:
		return "Basic"
	case vectorVariant:
		return "Vector"
	case structVariant:
		return "Struct"
	case unsupportedVariant:
		return "UNSUPPORTED"
	default:
		return fmt.Sprintf("invalid outerVariant %d", v)
	}
}

// panics on any values which aren't supported by dynfidl
// should be guarded by a call to `isSupported`
func visit(value ir.Value, decl mixer.Declaration) visitResult {
	switch value := value.(type) {
	case bool:
		return visitResult{
			ValueStr:            strconv.FormatBool(value),
			OuterVariant:        basicVariant,
			InnerEnumAndVariant: "BasicField::Bool",
		}
	case int64, uint64, float64:
		suffix, basicOuterVariant := primitiveTypeName(decl.(mixer.PrimitiveDeclaration).Subtype())
		return visitResult{
			ValueStr:            fmt.Sprintf("%v%s", value, suffix),
			OuterVariant:        basicVariant,
			InnerEnumAndVariant: fmt.Sprintf("BasicField::%s", basicOuterVariant),
		}
	case string:
		var valueStr string
		if fidlgen.PrintableASCII(value) {
			valueStr = fmt.Sprintf("String::from(%q).into_bytes()", value)
		} else {
			valueStr = fmt.Sprintf("b\"%s\".to_vec()", rust.EscapeStr(value))
		}
		return visitResult{
			ValueStr:            valueStr,
			OuterVariant:        vectorVariant,
			InnerEnumAndVariant: "VectorField::UInt8Vector",
		}
	case ir.Record:
		decl := decl.(*mixer.StructDecl)
		valueStr := "Structure::default()"
		for _, field := range value.Fields {
			fieldResult := visit(field.Value, decl.Field(field.Key.Name))
			if fieldResult.InnerEnumAndVariant != "" {
				valueStr += fmt.Sprintf(
					".field(Field::%s(%s(%s)))",
					fieldResult.OuterVariant,
					fieldResult.InnerEnumAndVariant,
					fieldResult.ValueStr)
			} else {
				valueStr += fmt.Sprintf(
					".field(Field::%s(%s))",
					fieldResult.OuterVariant,
					fieldResult.ValueStr)
			}
		}
		return visitResult{
			ValueStr:            valueStr,
			OuterVariant:        structVariant,
			InnerEnumAndVariant: "",
		}
	case []ir.Value:
		elemDecl := decl.(*mixer.VectorDecl).Elem()
		var elements []string
		for _, item := range value {
			visited := visit(item, elemDecl)
			elements = append(elements, visited.ValueStr)
		}
		valueStr := fmt.Sprintf("vec![%s]", strings.Join(elements, ", "))

		var innerEnumAndVariant string
		switch decl := elemDecl.(type) {
		case mixer.PrimitiveDeclaration:
			_, basicOuterVariant := primitiveTypeName(decl.Subtype())
			innerEnumAndVariant = fmt.Sprintf("VectorField::%sVector", basicOuterVariant)
		case *mixer.StringDecl, *mixer.VectorDecl:
			// vectors are only supported as vector elements if they're vector<uint8>
			// let rustc error if we're wrong about that assumption
			innerEnumAndVariant = "VectorField::UInt8VectorVector"
		default:
			panic(fmt.Sprintf("unexpected type for a vector element %s", decl))
		}

		return visitResult{
			ValueStr:            valueStr,
			OuterVariant:        vectorVariant,
			InnerEnumAndVariant: innerEnumAndVariant,
		}
	default:
		panic(fmt.Sprintf("unsupported type: %T", value))
	}
}

// panics on any values which aren't supported by dynfidl
// should be guarded by a call to `isSupported`
func primitiveTypeName(subtype fidlgen.PrimitiveSubtype) (string, string) {
	switch subtype {
	case fidlgen.Bool:
		return "bool", "Bool"
	case fidlgen.Int8:
		return "i8", "Int8"
	case fidlgen.Uint8:
		return "u8", "UInt8"
	case fidlgen.Int16:
		return "i16", "Int16"
	case fidlgen.Uint16:
		return "u16", "UInt16"
	case fidlgen.Int32:
		return "i32", "Int32"
	case fidlgen.Uint32:
		return "u32", "UInt32"
	case fidlgen.Int64:
		return "i64", "Int64"
	case fidlgen.Uint64:
		return "u64", "UInt64"
	default:
		panic(fmt.Sprintf("unsupported subtype %v", subtype))
	}
}
