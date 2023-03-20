// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package rust

import (
	"embed"
	"fmt"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"text/template"

	"go.fuchsia.dev/fuchsia/tools/fidl/lib/fidlgen"
	"go.fuchsia.dev/fuchsia/zircon/tools/zither"
)

//go:embed templates/*
var templates embed.FS

// Generator provides go data layout bindings.
type Generator struct {
	fidlgen.Generator
}

func NewGenerator(formatter fidlgen.Formatter) *Generator {
	gen := fidlgen.NewGenerator("RustTemplates", templates, formatter, template.FuncMap{
		"LowerCaseWithUnderscores": LowerCaseWithUnderscores,
		"UpperCaseWithUnderscores": zither.UpperCaseWithUnderscores,
		"UpperCamelCase":           zither.UpperCamelCase,
		"ScalarTypeName":           ScalarTypeName,
		"Imports":                  Imports,
		"ConstType":                ConstType,
		"ConstValue":               ConstValue,
		"EnumAttributes":           EnumAttributes,
		"StructAttributes":         StructAttributes,
		"DescribeType": func(desc zither.TypeDescriptor) string {
			return DescribeType(desc, CaseStyleRust)
		},
	})
	return &Generator{*gen}
}

func (gen Generator) DeclOrder() zither.DeclOrder { return zither.SourceDeclOrder }

func (gen *Generator) Generate(summaries []zither.FileSummary, outputDir string) ([]string, error) {
	lib := summaries[0].Library
	rootData := crateRootData{Library: lib}
	outputDir = filepath.Join(outputDir, strings.Join(lib.Parts(), "-"), "src")

	var outputs []string
	for _, summary := range summaries {
		rootData.Modules = append(rootData.Modules, summary.Name())
		output := filepath.Join(outputDir, summary.Name()+".rs")
		if err := gen.GenerateFile(output, "GenerateRustFile", summary); err != nil {
			return nil, err
		}
		outputs = append(outputs, output)
	}

	crateRoot := filepath.Join(outputDir, "lib.rs")
	sort.Strings(rootData.Modules) // Default `Modules` order may be nondeterministic.
	if err := gen.GenerateFile(crateRoot, "GenerateRustCrateRoot", rootData); err != nil {
		return nil, err
	}
	outputs = append(outputs, crateRoot)
	return outputs, nil
}

// crateRootData is data for the "GenerateRustCrateRoot" template.
type crateRootData struct {
	Library fidlgen.LibraryName
	Modules []string
}

//
// Template functions.
//

func LowerCaseWithUnderscores(el zither.Element) string {
	// Be sure to escape keywords.
	reservedKeywords := map[string]struct{}{
		"type": {},
	}
	val := zither.LowerCaseWithUnderscores(el)
	if _, ok := reservedKeywords[val]; ok {
		val = "r#" + val
	}
	return val
}

func ScalarTypeName(typ fidlgen.PrimitiveSubtype) string {
	switch typ {
	case fidlgen.Bool:
		return "bool"
	case fidlgen.Int8:
		return "i8"
	case fidlgen.Int16:
		return "i16"
	case fidlgen.Int32:
		return "i32"
	case fidlgen.Int64:
		return "i64"
	case fidlgen.Uint8, fidlgen.ZxExperimentalUchar:
		return "u8"
	case fidlgen.Uint16:
		return "u16"
	case fidlgen.Uint32:
		return "u32"
	case fidlgen.Uint64:
		return "u64"
	case fidlgen.ZxExperimentalUsize, fidlgen.ZxExperimentalUintptr:
		return "usize"
	default:
		panic(fmt.Sprintf("%s unknown FIDL primitive type: %s", typ))
	}
}

func Imports(summary zither.FileSummary) []string {
	var imports []string
	for _, kind := range summary.TypeKinds() {
		switch kind {
		case zither.TypeKindBits:
			imports = append(imports, "bitflags::bitflags")
		}
	}
	return imports
}

func ConstType(c zither.Const) string {
	switch c.Kind {
	case zither.TypeKindBool, zither.TypeKindInteger, zither.TypeKindSize:
		return ScalarTypeName(fidlgen.PrimitiveSubtype(c.Type))
	case zither.TypeKindString:
		return "&str"
	case zither.TypeKindEnum, zither.TypeKindBits:
		return zither.UpperCamelCase(c.Element.Decl)
	default:
		panic(fmt.Sprintf("%s has unknown constant kind: %s", c.Name, c.Type))
	}
}

func ConstValue(c zither.Const) string {
	if c.Element != nil {
		switch c.Kind {
		case zither.TypeKindEnum:
			return zither.UpperCamelCase(c.Element.Decl) + "::" + zither.UpperCamelCase(c.Element.Member)
		case zither.TypeKindBits:
			if c.Element.Member != nil {
				return zither.UpperCamelCase(c.Element.Decl) + "::" + zither.UpperCaseWithUnderscores(c.Element.Member)
			}
			val, err := strconv.Atoi(c.Value)
			if err != nil {
				panic(fmt.Sprintf("%s has malformed integral value: %s", c.Name, err))
			}
			return fmt.Sprintf("%s::from_bits_truncate(%#b)", zither.UpperCamelCase(c.Element.Decl), val)
		default:
			return zither.UpperCaseWithUnderscores(c.Element.Decl)
		}
	}

	switch c.Kind {
	case zither.TypeKindString:
		return fmt.Sprintf("%q", c.Value)
	case zither.TypeKindBool, zither.TypeKindInteger, zither.TypeKindSize:
		return c.Value
	case zither.TypeKindEnum, zither.TypeKindBits:
		// Enum and bits constants should have been handled above.
		panic(fmt.Sprintf("enum and bits constants must be given by an `Element` value: %#v", c))
	default:
		panic(fmt.Sprintf("%s has unknown constant kind: %s", c.Name, c.Type))
	}
}

// TODO(joshuaseaton): Add more.
func EnumAttributes(e zither.Enum) []string {
	return []string{fmt.Sprintf("#[repr(%s)]", ScalarTypeName(e.Subtype))}
}

// TODO(joshuaseaton): Add more.
func StructAttributes() []string {
	return []string{"#[repr(C)]"}
}

// CaseStyle represents a style of casing Rust type names.
type CaseStyle int

const (
	// CaseStyleRust represents official rust style.
	CaseStyleRust CaseStyle = iota

	// CaseStyleSyscall gives a C-style spelling for certain types, for
	// suggestive use in the Rust FFI syscall wrapper definitions.
	CaseStyleSyscall
)

func DescribeType(desc zither.TypeDescriptor, style CaseStyle) string {
	var casify func(string) string
	switch style {
	case CaseStyleRust:
		casify = fidlgen.ToUpperCamelCase
	case CaseStyleSyscall:
		casify = func(name string) string {
			typ := fidlgen.ToSnakeCase(name)
			switch desc.Kind {
			case zither.TypeKindAlias, zither.TypeKindStruct, zither.TypeKindEnum,
				zither.TypeKindBits, zither.TypeKindHandle:
				return "zx_" + typ + "_t"
			default:
				return typ
			}
		}
	}

	switch desc.Kind {
	case zither.TypeKindBool, zither.TypeKindInteger, zither.TypeKindSize:
		return ScalarTypeName(fidlgen.PrimitiveSubtype(desc.Type))
	case zither.TypeKindEnum, zither.TypeKindBits, zither.TypeKindStruct:
		layout, _ := fidlgen.MustReadName(desc.Type).SplitMember()
		return casify(layout.DeclarationName())
	case zither.TypeKindAlias, zither.TypeKindHandle:
		// TODO(fxbug.dev/105758): This assumes that the alias/handle was defined
		// within the same package. That's true now, but this would need to be
		// re-evaluated if/when zither supports library dependencies and the IR
		// preserves imported alias names.
		return casify(fidlgen.MustReadName(desc.Type).DeclarationName())
	case zither.TypeKindArray:
		return fmt.Sprintf("[%s; %d]", DescribeType(*desc.ElementType, style), *desc.ElementCount)
	case zither.TypeKindStringArray:
		return fmt.Sprintf("[u8; %d]", *desc.ElementCount)
	case zither.TypeKindPointer, zither.TypeKindVoidPointer:
		mutability := "const"
		if desc.ElementType.Mutable {
			mutability = "mut"
		}
		return fmt.Sprintf("*%s %s", mutability, DescribeType(*desc.ElementType, style))
	default:
		panic(fmt.Sprintf("unsupported type kind: %v", desc.Kind))
	}
}
