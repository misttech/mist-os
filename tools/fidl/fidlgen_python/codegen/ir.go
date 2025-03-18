// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package codegen

import (
	"fmt"
	"log"
	"strings"

	"go.fuchsia.dev/fuchsia/tools/fidl/lib/fidlgen"
)

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

type PythonAlias struct {
	fidlgen.Alias
	PythonAliasedName string
	PythonName        string
}

type PythonRoot struct {
	fidlgen.Root
	PythonModuleName string
	PythonStructs    []PythonStruct
	PythonAliases    []PythonAlias
}

type compiler struct {
	decls   fidlgen.DeclInfoMap
	library fidlgen.EncodedLibraryIdentifier
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
	} else {
		name := compileCamelIdentifier(ci.Name)
		return &name
	}
	return nil
}

func compileSnakeIdentifier(val fidlgen.Identifier) string {
	return fidlgen.ToSnakeCase(string(val))
}

type PythonType struct {
	fidlgen.Type
	PythonName string
}

func (c *compiler) compileType(val fidlgen.Type, maybeAlias *fidlgen.PartialTypeConstructor) *PythonType {
	name := ""
	if val.Nullable {
		name = "typing.Optional["
	}

	switch val.Kind {
	case fidlgen.IdentifierType:
		name += *c.compileDeclIdentifier(val.Identifier)
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

func Compile(root fidlgen.Root) PythonRoot {
	root = root.ForBindings("python")
	root = root.ForTransports([]string{"Channel"})
	python_root := PythonRoot{
		Root:             root,
		PythonModuleName: ToPythonModuleName(root.Name),
		PythonStructs:    []PythonStruct{},
	}
	c := compiler{
		decls:   root.DeclInfo(),
		library: root.Name,
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

	return python_root
}
