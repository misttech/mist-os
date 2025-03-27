// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package codegen

import (
	"fmt"
	"log"
	"sort"
	"strings"

	"go.fuchsia.dev/fuchsia/tools/fidl/lib/fidlgen"
)

type PythonType struct {
	fidlgen.Type
	PythonName string
}

type PythonBits struct {
	fidlgen.Bits
	PythonName    string
	PythonMembers []PythonBitsMember
	Empty         bool
}

type PythonBitsMember struct {
	fidlgen.BitsMember
	PythonName  string
	PythonValue string
}

type PythonEnum struct {
	fidlgen.Enum
	PythonName    string
	PythonMembers []PythonEnumMember
	Empty         bool
	HasZero       bool
}

type PythonEnumMember struct {
	fidlgen.EnumMember
	PythonName  string
	PythonValue string
}

type PythonTable struct {
	fidlgen.Table
	Library       string
	PythonName    string
	PythonMembers []PythonTableMember
}

type PythonTableMember struct {
	fidlgen.TableMember
	PythonType PythonType
	PythonName string
}

type PythonStruct struct {
	fidlgen.Struct
	Library       string
	PythonName    string
	PythonMembers []PythonStructMember
}

type PythonStructMember struct {
	fidlgen.StructMember
	PythonType PythonType
	PythonName string
}

type PythonUnion struct {
	fidlgen.Union
	Library       string
	PythonName    string
	PythonMembers []PythonUnionMember
}

type PythonUnionMember struct {
	fidlgen.UnionMember
	PythonType PythonType
	PythonName string
}

type PythonAlias struct {
	fidlgen.Alias
	PythonAliasedName string
	PythonName        string
}

type PythonRoot struct {
	fidlgen.Root
	PythonModuleName      string
	PythonTables          []PythonTable
	PythonStructs         []PythonStruct
	PythonUnions          []PythonUnion
	PythonAliases         []PythonAlias
	PythonBits            []PythonBits
	PythonEnums           []PythonEnum
	PythonExternalModules []string
}

type compiler struct {
	decls           fidlgen.DeclInfoMap
	library         fidlgen.EncodedLibraryIdentifier
	externalModules map[string]struct{}
}

func (c *compiler) lookupDeclInfo(val fidlgen.EncodedCompoundIdentifier) *fidlgen.DeclInfo {
	if info, ok := c.decls[val]; ok {
		return &info
	}
	log.Fatalf("Identifier missing from DeclInfoMap: %v", val)
	return nil
}

func compileCamelIdentifier(val fidlgen.Identifier) string {
	return fidlgen.ToUpperCamelCase(string(val))
}

func (c *compiler) inExternalLibrary(eci fidlgen.EncodedCompoundIdentifier) bool {
	return eci.LibraryName() != c.library
}

func ToPythonModuleName(eli fidlgen.EncodedLibraryIdentifier) string {
	return fmt.Sprintf("fidl_%s", strings.Join(eli.Parts(), "_"))
}

func (c *compiler) compileDeclIdentifier(val fidlgen.EncodedCompoundIdentifier) *string {
	ci := val.Parse()

	if ci.Member != "" {
		log.Fatalf("Non-empty Member implies this is not a declaration: %v", val)
	}

	if c.lookupDeclInfo(val).Type == fidlgen.ConstDeclType {
		log.Fatalf("ConstDeclType not supported")
		return nil
	}
	name := compileCamelIdentifier(ci.Name)

	if val.LibraryName() == c.library {
		return &name
	}

	externalModule := ToPythonModuleName(val.LibraryName())
	c.externalModules[externalModule] = struct{}{}
	externalName := fmt.Sprintf("%s.%s", externalModule, name)
	return &externalName
}

func compileSnakeIdentifier(val fidlgen.Identifier) string {
	return fidlgen.ToSnakeCase(string(val))
}

func compileScreamingSnakeIdentifier(val fidlgen.Identifier) string {
	return fidlgen.ConstNameToAllCapsSnake(string(val))
}

func (c *compiler) compileType(val fidlgen.Type, maybeAlias *fidlgen.PartialTypeConstructor) *PythonType {
	name := ""
	if val.Nullable {
		name = "typing.Optional["
	}

	switch val.Kind {
	case fidlgen.IdentifierType:
		// TODO(https://fxbug.dev/394421154): This should be changed to use the enum type itself
		// when we start making breaking changes for these bindings.
		switch c.decls[val.Identifier].Type {
		case fidlgen.BitsDeclType, fidlgen.EnumDeclType:
			name += "int"
		default:
			name += *c.compileDeclIdentifier(val.Identifier)
		}
	case fidlgen.PrimitiveType:
		subtype := string(val.PrimitiveSubtype)
		if strings.HasPrefix(subtype, "int") || strings.HasPrefix(subtype, "uint") {
			name += "int"
		} else if strings.HasPrefix(subtype, "float") {
			name += "float"
		} else if subtype == "bool" {
			name += "bool"
		} else {
			log.Fatalf("Unknown primitive subtype: %v", val)
		}
	case fidlgen.StringType:
		name += "str"
	case fidlgen.HandleType:
		name += "int"
	case fidlgen.VectorType, fidlgen.ArrayType:
		element_type_ptr := c.compileType(*val.ElementType, val.MaybeFromAlias)
		if element_type_ptr == nil {
			log.Fatalf("Element type not supported")
		}
		element_type := *element_type_ptr
		name += fmt.Sprintf("typing.Sequence[%s]", element_type.PythonName)
	default:
		log.Fatalf("Unknown kind: %v", val)
	}
	if val.Nullable {
		name += "]"
	}
	return &PythonType{
		Type:       val,
		PythonName: name,
	}
}

func (c *compiler) compileAlias(val fidlgen.Alias) PythonAlias {
	t := c.compileType(val.Type, val.MaybeFromAlias)
	if t == nil {
		log.Fatalf("Type not supported")
	}

	return PythonAlias{
		Alias:             val,
		PythonAliasedName: t.PythonName,
		PythonName:        *c.compileDeclIdentifier(val.Name),
	}
}

// TODO(https://fxbug.dev/396552135): Add literal tests to conformance test suite.
func (c *compiler) compileLiteral(val fidlgen.Literal) *string {
	switch val.Kind {
	case fidlgen.NumericLiteral:
		return &val.Value
	default:
		log.Fatalf("unknown literal kind: %v", val)
	}
	return nil
}

func (c *compiler) compileBits(val fidlgen.Bits) PythonBits {
	e := PythonBits{
		Bits:          val,
		PythonName:    *c.compileDeclIdentifier(val.Name),
		PythonMembers: []PythonBitsMember{},
		Empty:         len(val.Members) == 0,
	}
	for _, member_val := range val.Members {
		var value string
		switch member_val.Value.Kind {
		case fidlgen.LiteralConstant:
			value = *c.compileLiteral(*member_val.Value.Literal)
		default:
			log.Fatalf("Unknown bits member kind: %v", member_val)
		}

		e.PythonMembers = append(e.PythonMembers, PythonBitsMember{
			BitsMember:  member_val,
			PythonName:  compileScreamingSnakeIdentifier(member_val.Name),
			PythonValue: value,
		})
	}
	return e
}

func (c *compiler) compileEnum(val fidlgen.Enum) PythonEnum {
	e := PythonEnum{
		Enum:          val,
		PythonName:    *c.compileDeclIdentifier(val.Name),
		PythonMembers: []PythonEnumMember{},
		Empty:         len(val.Members) == 0,
		HasZero:       false,
	}
	for _, member_val := range val.Members {
		var value string
		switch member_val.Value.Kind {
		case fidlgen.LiteralConstant:
			value = *c.compileLiteral(*member_val.Value.Literal)
			e.HasZero = e.HasZero || (value == "0")
		default:
			log.Fatalf("Unknown enum member kind: %v", member_val)
		}

		e.PythonMembers = append(e.PythonMembers, PythonEnumMember{
			EnumMember:  member_val,
			PythonName:  compileScreamingSnakeIdentifier(member_val.Name),
			PythonValue: value,
		})
	}
	return e
}

func (c *compiler) compileTableMember(val fidlgen.TableMember) PythonTableMember {
	t := c.compileType(val.Type, val.MaybeFromAlias)
	if t == nil {
		log.Fatalf("Type not supported")
	}
	return PythonTableMember{
		TableMember: val,
		PythonType:  *t,
		PythonName:  compileSnakeIdentifier(val.Name),
	}
}

func (c *compiler) compileTable(val fidlgen.Table) PythonTable {
	name := *c.compileDeclIdentifier(val.Name)
	python_table := PythonTable{
		Table:         val,
		Library:       string(val.Name.LibraryName()),
		PythonName:    name,
		PythonMembers: []PythonTableMember{},
	}

	for _, v := range val.Members {
		member := c.compileTableMember(v)
		python_table.PythonMembers = append(python_table.PythonMembers, member)
	}

	return python_table
}

func (c *compiler) compileStructMember(val fidlgen.StructMember) PythonStructMember {
	t := c.compileType(val.Type, val.MaybeFromAlias)
	if t == nil {
		log.Fatalf("Type not supported")
	}
	return PythonStructMember{
		StructMember: val,
		PythonType:   *t,
		PythonName:   compileSnakeIdentifier(val.Name),
	}
}

func (c *compiler) compileStruct(val fidlgen.Struct) PythonStruct {
	name := *c.compileDeclIdentifier(val.Name)
	python_struct := PythonStruct{
		Struct:        val,
		Library:       string(val.Name.LibraryName()),
		PythonName:    name,
		PythonMembers: []PythonStructMember{},
	}

	for _, v := range val.Members {
		member := c.compileStructMember(v)
		python_struct.PythonMembers = append(python_struct.PythonMembers, member)
	}

	return python_struct
}

func (c *compiler) compileUnionMember(val fidlgen.UnionMember) PythonUnionMember {
	t := c.compileType(val.Type, val.MaybeFromAlias)
	if t == nil {
		log.Fatalf("Type not supported")
	}
	return PythonUnionMember{
		UnionMember: val,
		PythonType:  *t,
		PythonName:  compileSnakeIdentifier(val.Name),
	}
}

func (c *compiler) compileUnion(val fidlgen.Union) PythonUnion {
	name := *c.compileDeclIdentifier(val.Name)
	python_union := PythonUnion{
		Union:         val,
		Library:       string(val.Name.LibraryName()),
		PythonName:    name,
		PythonMembers: []PythonUnionMember{},
	}

	for _, v := range val.Members {
		member := c.compileUnionMember(v)
		python_union.PythonMembers = append(python_union.PythonMembers, member)
	}

	return python_union
}

func Compile(root fidlgen.Root) PythonRoot {
	root = root.ForBindings("python")
	root = root.ForTransports([]string{"Channel"})
	python_root := PythonRoot{
		Root:             root,
		PythonModuleName: ToPythonModuleName(root.Name),
		PythonStructs:    []PythonStruct{},
		PythonBits:       []PythonBits{},
	}
	c := compiler{
		decls:           root.DeclInfo(),
		library:         root.Name,
		externalModules: map[string]struct{}{},
	}

	for _, v := range root.Tables {
		python_root.PythonTables = append(python_root.PythonTables, c.compileTable(v))
	}

	for _, v := range root.Structs {
		if v.IsEmptySuccessStruct {
			continue
		}
		python_root.PythonStructs = append(python_root.PythonStructs, c.compileStruct(v))
	}

	for _, v := range root.Aliases {
		python_root.PythonAliases = append(python_root.PythonAliases, c.compileAlias(v))
	}

	// TODO(https://fxbug.dev/396552135): Extend the test suite to cover the different variations
	// of bits declarations.
	for _, v := range root.Bits {
		python_root.PythonBits = append(python_root.PythonBits, c.compileBits(v))
	}

	for _, v := range root.Enums {
		python_root.PythonEnums = append(python_root.PythonEnums, c.compileEnum(v))
	}

	for _, v := range root.Unions {
		// TODO(https://fxbug.dev/394421154): If v is a result, then its creation should
		// be deferred to when the corresponding protocol method is compiled.
		python_root.PythonUnions = append(python_root.PythonUnions, c.compileUnion(v))
	}

	// Sort the external modules to make sure the generated file is
	// consistent across builds.
	var externalModules []string
	for k := range c.externalModules {
		externalModules = append(externalModules, k)
	}
	sort.Strings(externalModules)

	for _, k := range externalModules {
		python_root.PythonExternalModules = append(python_root.PythonExternalModules, k)
	}

	return python_root
}
