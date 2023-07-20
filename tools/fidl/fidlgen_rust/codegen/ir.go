// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package codegen

import (
	"encoding/binary"
	"fmt"
	"math"
	"sort"
	"strconv"
	"strings"
	"unicode"

	"go.fuchsia.dev/fuchsia/tools/fidl/lib/fidlgen"
)

type EncodedCompoundIdentifier = fidlgen.EncodedCompoundIdentifier

type Type struct {
	// TODO(fxbug.dev/7660): Remove Resourceness once stored on fidlgen.Type.
	fidlgen.Resourceness

	// Information extracted from fidlgen.Type.
	Kind             fidlgen.TypeKind
	Nullable         bool
	PrimitiveSubtype fidlgen.PrimitiveSubtype
	ElementType      *Type
	Identifier       EncodedCompoundIdentifier
	DeclType         fidlgen.DeclType

	// The marker type that implements fidl::encoding::Type.
	Fidl string
	// The associated type fidl::encoding::Type::Owned.
	Owned string
	// The type to use when this occurs as a method parameter.
	// TODO(fxbug.dev/122199): Once the transition to the new types if complete,
	// document this as being {Value,Resource}Type::Borrowed.
	Param string
}

type Bits struct {
	fidlgen.Bits
	Name           string
	UnderlyingType string
	Members        []BitsMember
}

type BitsMember struct {
	fidlgen.BitsMember
	Name  string
	Value string
}

type Const struct {
	fidlgen.Const
	Name  string
	Type  string
	Value string
}

type Enum struct {
	fidlgen.Enum
	Name           string
	UnderlyingType string
	Members        []EnumMember
	// Member name with the minimum value, used as an arbitrary default value
	// in Decode::new_empty for strict enums.
	MinMember string
}

type EnumMember struct {
	fidlgen.EnumMember
	Name  string
	Value string
}

type Union struct {
	fidlgen.Union
	Derives derives
	ECI     EncodedCompoundIdentifier
	Name    string
	Members []UnionMember
}

type UnionMember struct {
	fidlgen.UnionMember
	Type    Type
	Name    string
	Ordinal int
}

type Struct struct {
	fidlgen.Struct
	ECI                                                  EncodedCompoundIdentifier
	Derives                                              derives
	Name                                                 string
	Members                                              []StructMember
	PaddingMarkersV1, PaddingMarkersV2                   []rustPaddingMarker
	FlattenedPaddingMarkersV1, FlattenedPaddingMarkersV2 []rustPaddingMarker
	SizeV1, SizeV2                                       int
	AlignmentV1, AlignmentV2                             int
	HasPadding                                           bool
	// True if the struct should be encoded and decoded by memcpy.
	UseFidlStructCopy bool
}

type StructMember struct {
	fidlgen.StructMember
	Type               Type
	Name               string
	OffsetV1, OffsetV2 int
}

type Table struct {
	fidlgen.Table
	Derives derives
	ECI     EncodedCompoundIdentifier
	Name    string
	Members []TableMember
}

func (t *Table) ReversedMembers() []TableMember {
	var r []TableMember
	for i := len(t.Members) - 1; i >= 0; i-- {
		r = append(r, t.Members[i])
	}
	return r
}

type TableMember struct {
	fidlgen.TableMember
	Type    Type
	Name    string
	Ordinal int
}

// Protocol is the definition of a protocol in the library being compiled.
type Protocol struct {
	// Raw JSON IR data about this protocol. Embedded to provide access to
	// fields common to all bindings.
	fidlgen.Protocol
	// Compound identifier referring to this protocol.
	ECI EncodedCompoundIdentifier
	// Name of the protocol as a Rust CamelCase identifier. Since only protocols
	// from the same library are included, this will never be qualified, so it
	// is just the CamelCase name of the protocol.
	Name string
	// List of methods that are part of this protocol. Processed from
	// fidlgen.Protocol to add Rust-specific fields.
	Methods []Method
	// Name of this protocol for legacy (pre-RFC-0041) service discovery, if the
	// protocol is marked as discoverable. This value does not include enclosing
	// quote marks.
	ProtocolName string
}

// Method is a method defined in a protocol.
type Method struct {
	// Raw JSON IR data about this method. Embedded to provide access to fields
	// common to all bindings.
	fidlgen.Method
	// Name of the method converted to snake_case. Used when generating
	// rust-methods associated with this method, such as proxy methods and
	// encoder methods.
	Name string
	// Name of the method converted to CamelCase. Used when generating
	// rust-types associated with this method, such as responders.
	CamelName string

	Request  Payload
	Response Payload
}

// DynamicFlags gets rust code for the DynamicFlags value that should be set for
// a call to this method.
func (m *Method) DynamicFlags() string {
	if m.IsStrict() {
		return "fidl::encoding::DynamicFlags::empty()"
	}
	return "fidl::encoding::DynamicFlags::FLEXIBLE"
}

// A method request or response.
type Payload struct {
	// The fidl::encoding::Type type. It can be fidl::encoding::EmptyPayload,
	// a struct, table, or union, or (only for two-way responses) one of
	// fidl::encoding::{Result,Flexible,FlexibleResult}Type.
	FidlType string
	// Equivalent to <FidlType as fidl::encoding::Type>::Owned.
	OwnedType string
	// The user-facing owned type. This is derived from OwnedType by removing
	// flexible wrappers (if any) and flattening structs to tuples.
	TupleType string
	// For methods that use error syntax, TupleType is an alias that refers
	// to TupleTypeAliasRhs. Otherwise, TupleTypeAliasRhs is empty.
	// TODO(fxbug.dev/122199): Remove.
	TupleTypeAliasRhs string
	// Parameters for sending this payload. For an empty payload, this is nil.
	// For a struct without error syntax, it contains one element per struct
	// field. Otherwise, it contains a single element (table, union, or Result).
	Parameters []Parameter
	// Assuming that names from Parameters are in scope, EncodeExpr is an
	// expression of a type implementing fidl::encoding::Encode<FidlType>.
	EncodeExpr string
	// ConvertToTuple converts an expression to TupleType. If OwnedType is
	// fidl::encoding::Flexible or fidl::encoding::FlexibleResult, expects the
	// input variable to be the Ok return value of .into_result(). Otherwise,
	// expects the input variable to be of type OwnedType.
	// TODO(fxbug.dev/122199): Remove.
	ConvertToTuple func(string) string
	// Like ConvertToTuple, but uses names from Parameters for inclusion in a
	// struct. Examples for ConvertToFields("v"):
	//     ""
	//     "param1: v.param1, param2: v.param2,"
	//     "result: v.map(|x| x.param1),"
	// TODO(fxbug.dev/122199): Remove.
	ConvertToFields func(string) string
}

// A parameter in a method request or response.
type Parameter struct {
	// Snake-case parameter name.
	Name string
	// Parameter type.
	Type string
	// Type to use when storing the parameter in an owning data structure.
	OwnedType string
	// Set only if this parameter corresponds to a struct member.
	StructMemberType *Type
}

type Service struct {
	fidlgen.Service
	Name        string
	Members     []ServiceMember
	ServiceName string
}

type ServiceMember struct {
	fidlgen.ServiceMember
	ProtocolType      string
	Name              string
	CamelName         string
	SnakeName         string
	ProtocolTransport string
}

type Root struct {
	Experiments  fidlgen.Experiments
	ExternCrates []string
	Bits         []Bits
	Consts       []Const
	Enums        []Enum
	Structs      []Struct
	Unions       []Union
	Tables       []Table
	Protocols    []Protocol
	Services     []Service
}

func (r *Root) findProtocol(eci EncodedCompoundIdentifier) *Protocol {
	for i := range r.Protocols {
		if r.Protocols[i].ECI == eci {
			return &r.Protocols[i]
		}
	}
	return nil
}

func (r *Root) findStruct(eci EncodedCompoundIdentifier) *Struct {
	for i := range r.Structs {
		if r.Structs[i].ECI == eci {
			return &r.Structs[i]
		}
	}
	return nil
}

func (r *Root) findTable(eci EncodedCompoundIdentifier) *Table {
	for i := range r.Tables {
		if r.Tables[i].ECI == eci {
			return &r.Tables[i]
		}
	}
	return nil
}

func (r *Root) findUnion(eci EncodedCompoundIdentifier) *Union {
	for i := range r.Unions {
		if r.Unions[i].ECI == eci {
			return &r.Unions[i]
		}
	}
	return nil
}

// ServicesForTransport returns services containing exclusively protocol
// members defined over the specified transport.
func (r Root) ServicesForTransport() func(string) []Service {
	return func(t string) []Service {
		var ss []Service
		for _, s := range r.Services {
			allOk := true
			for _, m := range s.Members {
				if m.ProtocolTransport != t {
					allOk = false
					break
				}
			}
			if allOk {
				ss = append(ss, s)
			}
		}
		return ss
	}
}

var reservedWords = map[string]struct{}{
	"as":       {},
	"box":      {},
	"break":    {},
	"const":    {},
	"continue": {},
	"crate":    {},
	"else":     {},
	"enum":     {},
	"extern":   {},
	"false":    {},
	"fn":       {},
	"for":      {},
	"if":       {},
	"impl":     {},
	"in":       {},
	"let":      {},
	"loop":     {},
	"match":    {},
	"mod":      {},
	"move":     {},
	"mut":      {},
	"pub":      {},
	"ref":      {},
	"return":   {},
	"self":     {},
	"Self":     {},
	"static":   {},
	"struct":   {},
	"super":    {},
	"trait":    {},
	"true":     {},
	"type":     {},
	"unsafe":   {},
	"use":      {},
	"where":    {},
	"while":    {},

	// Keywords reserved for future use (future-proofing...)
	"abstract": {},
	"alignof":  {},
	"await":    {},
	"become":   {},
	"do":       {},
	"final":    {},
	"macro":    {},
	"offsetof": {},
	"override": {},
	"priv":     {},
	"proc":     {},
	"pure":     {},
	"sizeof":   {},
	"typeof":   {},
	"unsized":  {},
	"virtual":  {},
	"yield":    {},

	// Weak keywords (special meaning in specific contexts)
	// These are ok in all contexts of FIDL names.
	//"default":	{},
	//"union":	{},

	// Things that are not keywords, but for which collisions would be very
	// unpleasant
	"Result":  {},
	"Ok":      {},
	"Err":     {},
	"Vec":     {},
	"Option":  {},
	"Some":    {},
	"None":    {},
	"Box":     {},
	"Future":  {},
	"Stream":  {},
	"Never":   {},
	"Send":    {},
	"fidl":    {},
	"futures": {},
	"zx":      {},
	"async":   {},
	"on_open": {},
	"OnOpen":  {},
	// TODO(fxbug.dev/66767): Remove "WaitForEvent".
	"wait_for_event": {},
	"WaitForEvent":   {},
}

var reservedSuffixes = []string{
	"Impl",
	"Marker",
	"Proxy",
	"ProxyProtocol",
	"ControlHandle",
	"Responder",
	"Server",
}

func isReservedWord(str string) bool {
	_, ok := reservedWords[str]
	return ok
}

// hasReservedSuffix checks if a string ends with a suffix commonly used by the
// bindings in generated types
func hasReservedSuffix(str string) bool {
	for _, suffix := range reservedSuffixes {
		if strings.HasSuffix(str, suffix) {
			return true
		}
	}
	return false
}

// changeIfReserved adds an underscore suffix to differentiate an identifier
// from a reserved name
//
// Reserved names include a variety of rust keywords, commonly used rust types
// like Result, Vec, and Future, and any name ending in a suffix used by the
// bindings to identify particular generated types like -Impl, -Marker, and
// -Proxy.
func changeIfReserved(val fidlgen.Identifier) string {
	str := string(val)
	if hasReservedSuffix(str) || isReservedWord(str) {
		return str + "_"
	}
	return str
}

var primitiveTypes = map[fidlgen.PrimitiveSubtype]string{
	fidlgen.Bool:    "bool",
	fidlgen.Int8:    "i8",
	fidlgen.Int16:   "i16",
	fidlgen.Int32:   "i32",
	fidlgen.Int64:   "i64",
	fidlgen.Uint8:   "u8",
	fidlgen.Uint16:  "u16",
	fidlgen.Uint32:  "u32",
	fidlgen.Uint64:  "u64",
	fidlgen.Float32: "f32",
	fidlgen.Float64: "f64",
}

var handleSubtypes = map[fidlgen.HandleSubtype]string{
	fidlgen.HandleSubtypeBti:          "fidl::Bti",
	fidlgen.HandleSubtypeChannel:      "fidl::Channel",
	fidlgen.HandleSubtypeClock:        "fidl::Clock",
	fidlgen.HandleSubtypeDebugLog:     "fidl::DebugLog",
	fidlgen.HandleSubtypeEvent:        "fidl::Event",
	fidlgen.HandleSubtypeEventpair:    "fidl::EventPair",
	fidlgen.HandleSubtypeException:    "fidl::Exception",
	fidlgen.HandleSubtypeFifo:         "fidl::Fifo",
	fidlgen.HandleSubtypeGuest:        "fidl::Guest",
	fidlgen.HandleSubtypeInterrupt:    "fidl::Interrupt",
	fidlgen.HandleSubtypeIommu:        "fidl::Iommu",
	fidlgen.HandleSubtypeJob:          "fidl::Job",
	fidlgen.HandleSubtypeMsi:          "fidl::Msi",
	fidlgen.HandleSubtypeNone:         "fidl::Handle",
	fidlgen.HandleSubtypePager:        "fidl::Pager",
	fidlgen.HandleSubtypePciDevice:    "fidl::PciDevice",
	fidlgen.HandleSubtypePmt:          "fidl::Pmt",
	fidlgen.HandleSubtypePort:         "fidl::Port",
	fidlgen.HandleSubtypeProcess:      "fidl::Process",
	fidlgen.HandleSubtypeProfile:      "fidl::Profile",
	fidlgen.HandleSubtypeResource:     "fidl::Resource",
	fidlgen.HandleSubtypeSocket:       "fidl::Socket",
	fidlgen.HandleSubtypeStream:       "fidl::Stream",
	fidlgen.HandleSubtypeSuspendToken: "fidl::SuspendToken",
	fidlgen.HandleSubtypeThread:       "fidl::Thread",
	fidlgen.HandleSubtypeTimer:        "fidl::Timer",
	fidlgen.HandleSubtypeVcpu:         "fidl::Vcpu",
	fidlgen.HandleSubtypeVmar:         "fidl::Vmar",
	fidlgen.HandleSubtypeVmo:          "fidl::Vmo",
}

var objectTypeConsts = map[fidlgen.HandleSubtype]string{
	fidlgen.HandleSubtypeBti:          "fidl::ObjectType::BTI",
	fidlgen.HandleSubtypeChannel:      "fidl::ObjectType::CHANNEL",
	fidlgen.HandleSubtypeClock:        "fidl::ObjectType::CLOCK",
	fidlgen.HandleSubtypeDebugLog:     "fidl::ObjectType::DEBUGLOG",
	fidlgen.HandleSubtypeEvent:        "fidl::ObjectType::EVENT",
	fidlgen.HandleSubtypeEventpair:    "fidl::ObjectType::EVENTPAIR",
	fidlgen.HandleSubtypeException:    "fidl::ObjectType::EXCEPTION",
	fidlgen.HandleSubtypeFifo:         "fidl::ObjectType::FIFO",
	fidlgen.HandleSubtypeGuest:        "fidl::ObjectType::GUEST",
	fidlgen.HandleSubtypeInterrupt:    "fidl::ObjectType::INTERRUPT",
	fidlgen.HandleSubtypeIommu:        "fidl::ObjectType::IOMMU",
	fidlgen.HandleSubtypeJob:          "fidl::ObjectType::JOB",
	fidlgen.HandleSubtypeMsi:          "fidl::ObjectType::MSI",
	fidlgen.HandleSubtypeNone:         "fidl::ObjectType::NONE",
	fidlgen.HandleSubtypePager:        "fidl::ObjectType::PAGER",
	fidlgen.HandleSubtypePciDevice:    "fidl::ObjectType::PCI_DEVICE",
	fidlgen.HandleSubtypePmt:          "fidl::ObjectType::PMT",
	fidlgen.HandleSubtypePort:         "fidl::ObjectType::PORT",
	fidlgen.HandleSubtypeProcess:      "fidl::ObjectType::PROCESS",
	fidlgen.HandleSubtypeProfile:      "fidl::ObjectType::PROFILE",
	fidlgen.HandleSubtypeResource:     "fidl::ObjectType::RESOURCE",
	fidlgen.HandleSubtypeSocket:       "fidl::ObjectType::SOCKET",
	fidlgen.HandleSubtypeStream:       "fidl::ObjectType::STREAM",
	fidlgen.HandleSubtypeSuspendToken: "fidl::ObjectType::SUSPEND_TOKEN",
	fidlgen.HandleSubtypeThread:       "fidl::ObjectType::THREAD",
	fidlgen.HandleSubtypeTimer:        "fidl::ObjectType::TIMER",
	fidlgen.HandleSubtypeVcpu:         "fidl::ObjectType::VCPU",
	fidlgen.HandleSubtypeVmar:         "fidl::ObjectType::VMAR",
	fidlgen.HandleSubtypeVmo:          "fidl::ObjectType::VMO",
}

type compiler struct {
	decls        fidlgen.DeclInfoMap
	experiments  fidlgen.Experiments
	library      fidlgen.LibraryIdentifier
	externCrates map[string]struct{}
	// Raw structs (including ExternalStructs), needed for
	// flattening parameters and in computeUseFidlStructCopy.
	structs map[fidlgen.EncodedCompoundIdentifier]fidlgen.Struct
}

// inExternalLibrary returns true if the library that the given
// CompoundIdentifier is in is different from the one the compiler is generating
// code for.
func (c *compiler) inExternalLibrary(ci fidlgen.CompoundIdentifier) bool {
	if len(ci.Library) != len(c.library) {
		return true
	}
	for i, part := range c.library {
		if ci.Library[i] != part {
			return true
		}
	}
	return false
}

// TODO(fxbug.dev/66767): Escaping reserved words should happen *after*
// converting to CamelCase.
func compileCamelIdentifier(val fidlgen.Identifier) string {
	return fidlgen.ToUpperCamelCase(changeIfReserved(val))
}

func compileLibraryName(library fidlgen.LibraryIdentifier) string {
	parts := []string{"fidl"}
	for _, part := range library {
		parts = append(parts, string(part))
	}
	return changeIfReserved(fidlgen.Identifier(strings.Join(parts, "_")))
}

// compileSnakeIdentifier converts the identifier to snake_case and escapes it
// (by adding an underscore suffix) if it collides with a reserved word.
// TODO(fxbug.dev/66767): Escaping reserved words should happen *after*
// converting to snake_case.
func compileSnakeIdentifier(val fidlgen.Identifier) string {
	return fidlgen.ToSnakeCase(changeIfReserved(val))
}

// TODO(fxbug.dev/66767): Escaping reserved words should happen *after*
// converting to SCREAMING_SNAKE_CASE.
func compileScreamingSnakeIdentifier(val fidlgen.Identifier) string {
	return fidlgen.ConstNameToAllCapsSnake(changeIfReserved(val))
}

// compileCompoundIdentifier produces a string Rust identifier which can be used
// from the generated code to refer to the specified FIDL declaration or member.
//
// The case used in the Declaration and Member names will be unchanged.
//
// If the CompoundIdentifier is from the current library, the name will be
// unqualified, meaning that it is suitable for use in Rust type declarations.
// If it is from a different library, it will be fully qualified, and the source
// library will be added as a required extern-crate.
func (c *compiler) compileCompoundIdentifier(val fidlgen.CompoundIdentifier) string {
	strs := []string{}
	if c.inExternalLibrary(val) {
		externName := compileLibraryName(val.Library)
		c.externCrates[externName] = struct{}{}
		strs = append(strs, externName)
	}
	str := changeIfReserved(val.Name)
	strs = append(strs, str)
	if val.Member != "" {
		strs = append(strs, string(val.Member))
	}
	return strings.Join(strs, "::")
}

// compileCamelCompoundIdentifier produces a string Rust identifier which can be
// used from the generated code to refer to the specified FIDL declaration or
// member.
//
// The resulting string will have the Declaration changed to CamelCase, but
// Member (if any) will be unaffected by case conversion.
//
// If the CompoundIdentifier is from the current library, the name will be
// unqualified. If the CompoundIdentifier refers to a Declaration, that means
// that the name is just the declaration name converted to CamelCase, so it is
// suitable for use in Rust type declarations. If the CompoundIdentifier is for
// a member within a declaration, the member name will be qualified by the
// declaration name, so it is not suitable for declaring e.g. a method name.
//
// If it is from a different library, it will be fully qualified, and the source
// library will be added as a required extern-crate.
func (c *compiler) compileCamelCompoundIdentifier(eci fidlgen.EncodedCompoundIdentifier) string {
	val := eci.Parse()
	val.Name = fidlgen.Identifier(compileCamelIdentifier(val.Name))
	return c.compileCompoundIdentifier(val)
}

func (c *compiler) compileSnakeCompoundIdentifier(eci fidlgen.EncodedCompoundIdentifier) string {
	val := eci.Parse()
	val.Name = fidlgen.Identifier(compileSnakeIdentifier(val.Name))
	return c.compileCompoundIdentifier(val)
}

func (c *compiler) compileScreamingSnakeCompoundIdentifier(eci fidlgen.EncodedCompoundIdentifier) string {
	val := eci.Parse()
	val.Name = fidlgen.Identifier(compileScreamingSnakeIdentifier(val.Name))
	return c.compileCompoundIdentifier(val)
}

func compileLiteral(val fidlgen.Literal, typ fidlgen.Type) string {
	switch val.Kind {
	case fidlgen.StringLiteral:
		var b strings.Builder
		b.WriteRune('"')
		for _, r := range val.Value {
			switch r {
			case '\\':
				b.WriteString(`\\`)
			case '"':
				b.WriteString(`\"`)
			case '\n':
				b.WriteString(`\n`)
			case '\r':
				b.WriteString(`\r`)
			case '\t':
				b.WriteString(`\t`)
			default:
				if unicode.IsPrint(r) {
					b.WriteRune(r)
				} else {
					b.WriteString(fmt.Sprintf(`\u{%x}`, r))
				}
			}
		}
		b.WriteRune('"')
		return b.String()
	case fidlgen.NumericLiteral:
		if typ.Kind == fidlgen.PrimitiveType &&
			(typ.PrimitiveSubtype == fidlgen.Float32 || typ.PrimitiveSubtype == fidlgen.Float64) {
			if !strings.ContainsRune(val.Value, '.') {
				return fmt.Sprintf("%s.0", val.Value)
			}
			return val.Value
		}
		return val.Value
	case fidlgen.BoolLiteral:
		return val.Value
	case fidlgen.DefaultLiteral:
		return "::Default::default()"
	default:
		panic(fmt.Sprintf("unknown literal kind: %v", val.Kind))
	}
}

func (c *compiler) identifierConstantDeclType(eci EncodedCompoundIdentifier) fidlgen.DeclType {
	memberless := eci.Parse()
	memberless.Member = ""
	declInfo, ok := c.decls[memberless.Encode()]
	if !ok {
		panic(fmt.Sprintf("identifier not in decl map: %s", memberless.Encode()))
	}
	return declInfo.Type
}

func (c *compiler) compileConstant(val fidlgen.Constant, typ fidlgen.Type) string {
	switch val.Kind {
	case fidlgen.IdentifierConstant:
		declType := c.identifierConstantDeclType(val.Identifier)
		parts := val.Identifier.Parse()
		switch declType {
		case fidlgen.ConstDeclType:
			parts.Name = fidlgen.Identifier(compileScreamingSnakeIdentifier(parts.Name))
			return c.compileCompoundIdentifier(parts)
		case fidlgen.BitsDeclType:
			parts.Name = fidlgen.Identifier(compileCamelIdentifier(parts.Name))
			parts.Member = fidlgen.Identifier(compileScreamingSnakeIdentifier(parts.Member))
			// TODO(fxbug.dev/93195): For now we assume the primitive type
			// matches the bits underlying type. If it doesn't the generated
			// Rust code will not compile.
			if typ.Kind == fidlgen.PrimitiveType {
				return fmt.Sprintf("%s.bits()", c.compileCompoundIdentifier(parts))
			}
			return c.compileCompoundIdentifier(parts)
		case fidlgen.EnumDeclType:
			parts.Name = fidlgen.Identifier(compileCamelIdentifier(parts.Name))
			parts.Member = fidlgen.Identifier(compileCamelIdentifier(parts.Member))
			// TODO(fxbug.dev/93195): For now we assume the primitive type
			// matches the enum underlying type. If it doesn't the generated
			// Rust code will not compile.
			if typ.Kind == fidlgen.PrimitiveType {
				return fmt.Sprintf("%s.into_primitive()", c.compileCompoundIdentifier(parts))
			}
			return c.compileCompoundIdentifier(parts)
		default:
			panic(fmt.Sprintf("unexpected decl type %s", declType))
		}
	case fidlgen.LiteralConstant:
		return compileLiteral(*val.Literal, typ)
	case fidlgen.BinaryOperator:
		if typ.Kind == fidlgen.PrimitiveType {
			return val.Value
		}
		decl := c.compileCamelCompoundIdentifier(typ.Identifier)
		// from_bits isn't a const function, so from_bits_truncate must be used.
		return fmt.Sprintf("%s::from_bits_truncate(%s)", decl, val.Value)
	default:
		panic(fmt.Sprintf("unknown constant kind: %s", val.Kind))
	}
}

func (c *compiler) compileConst(val fidlgen.Const) Const {
	name := c.compileScreamingSnakeCompoundIdentifier(val.Name)
	var r Const
	if val.Type.Kind == fidlgen.StringType {
		r = Const{
			Const: val,
			Type:  "&str",
			Name:  name,
			Value: c.compileConstant(val.Value, val.Type),
		}
	} else {
		r = Const{
			Const: val,
			Type:  c.compileType(val.Type).Owned,
			Name:  name,
			Value: c.compileConstant(val.Value, val.Type),
		}
	}
	return r
}

func compilePrimitiveSubtype(val fidlgen.PrimitiveSubtype) string {
	if t, ok := primitiveTypes[val]; ok {
		return t
	}
	panic(fmt.Sprintf("unknown primitive type: %v", val))
}

func compileHandleSubtype(val fidlgen.HandleSubtype) string {
	if t, ok := handleSubtypes[val]; ok {
		return t
	}
	panic(fmt.Sprintf("unknown handle type: %v", val))
}

func compileObjectTypeConst(val fidlgen.HandleSubtype) string {
	if t, ok := objectTypeConsts[val]; ok {
		return t
	}
	panic(fmt.Sprintf("unknown handle type: %v", val))
}

func (c *compiler) compileType(val fidlgen.Type) Type {
	t := Type{
		Resourceness:     c.decls.LookupResourceness(val),
		Kind:             val.Kind,
		Nullable:         val.Nullable,
		PrimitiveSubtype: val.PrimitiveSubtype,
		Identifier:       val.Identifier,
	}

	switch val.Kind {
	case fidlgen.PrimitiveType:
		s := compilePrimitiveSubtype(val.PrimitiveSubtype)
		t.Fidl = s
		t.Owned = s
		t.Param = s
	case fidlgen.ArrayType:
		el := c.compileType(*val.ElementType)
		t.ElementType = &el
		t.Fidl = fmt.Sprintf("fidl::encoding::Array<%s, %d>", el.Fidl, *val.ElementCount)
		t.Owned = fmt.Sprintf("[%s; %d]", el.Owned, *val.ElementCount)
		if el.IsResourceType() {
			t.Param = t.Owned
		} else {
			t.Param = "&" + t.Owned
		}
	case fidlgen.VectorType:
		el := c.compileType(*val.ElementType)
		t.ElementType = &el
		if val.ElementCount == nil {
			t.Fidl = fmt.Sprintf("fidl::encoding::UnboundedVector<%s>", el.Fidl)
		} else {
			t.Fidl = fmt.Sprintf("fidl::encoding::Vector<%s, %d>", el.Fidl, *val.ElementCount)
		}
		t.Owned = fmt.Sprintf("Vec<%s>", el.Owned)
		if el.IsResourceType() {
			t.Param = fmt.Sprintf("Vec<%s>", el.Owned)
		} else {
			t.Param = fmt.Sprintf("&[%s]", el.Owned)
		}
		if val.Nullable {
			t.Fidl = fmt.Sprintf("fidl::encoding::Optional<%s>", t.Fidl)
			t.Owned = fmt.Sprintf("Option<%s>", t.Owned)
			t.Param = fmt.Sprintf("Option<%s>", t.Param)
		}
	case fidlgen.StringType:
		if val.ElementCount == nil {
			t.Fidl = "fidl::encoding::UnboundedString"
		} else {
			t.Fidl = fmt.Sprintf("fidl::encoding::BoundedString<%d>", *val.ElementCount)
		}
		t.Owned = "String"
		t.Param = "&str"
		if val.Nullable {
			t.Fidl = fmt.Sprintf("fidl::encoding::Optional<%s>", t.Fidl)
			t.Owned = fmt.Sprintf("Option<%s>", t.Owned)
			t.Param = fmt.Sprintf("Option<%s>", t.Param)
		}
	case fidlgen.HandleType:
		s := compileHandleSubtype(val.HandleSubtype)
		objType := compileObjectTypeConst(val.HandleSubtype)
		t.Fidl = fmt.Sprintf("fidl::encoding::HandleType<%s, { %s.into_raw() }, %d>", s, objType, val.HandleRights)
		t.Owned = s
		t.Param = s
		if val.Nullable {
			t.Fidl = fmt.Sprintf("fidl::encoding::Optional<%s>", t.Fidl)
			t.Owned = fmt.Sprintf("Option<%s>", t.Owned)
			t.Param = fmt.Sprintf("Option<%s>", t.Param)
		}
	case fidlgen.RequestType:
		s := fmt.Sprintf("fidl::endpoints::ServerEnd<%sMarker>", c.compileCamelCompoundIdentifier(val.RequestSubtype))
		t.Fidl = fmt.Sprintf("fidl::encoding::Endpoint<%s>", s)
		t.Owned = s
		t.Param = s
		if val.Nullable {
			t.Fidl = fmt.Sprintf("fidl::encoding::Optional<%s>", t.Fidl)
			t.Owned = fmt.Sprintf("Option<%s>", t.Owned)
			t.Param = fmt.Sprintf("Option<%s>", t.Param)
		}
	case fidlgen.IdentifierType:
		name := c.compileCamelCompoundIdentifier(val.Identifier)
		declInfo, ok := c.decls[val.Identifier]
		if !ok {
			panic(fmt.Sprintf("unknown identifier: %v", val.Identifier))
		}
		t.DeclType = declInfo.Type
		switch declInfo.Type {
		case fidlgen.BitsDeclType, fidlgen.EnumDeclType:
			t.Fidl = name
			t.Owned = name
			t.Param = name
		case fidlgen.StructDeclType, fidlgen.TableDeclType, fidlgen.UnionDeclType:
			t.Fidl = name
			t.Owned = name
			if t.IsResourceType() {
				t.Param = name
			} else {
				t.Param = "&" + name
			}
			if val.Nullable {
				switch declInfo.Type {
				case fidlgen.StructDeclType:
					t.Fidl = fmt.Sprintf("fidl::encoding::Boxed<%s>", t.Fidl)
				case fidlgen.UnionDeclType:
					t.Fidl = fmt.Sprintf("fidl::encoding::OptionalUnion<%s>", t.Fidl)
				default:
					panic(fmt.Sprintf("unexpected type: %s", declInfo.Type))
				}
				t.Owned = fmt.Sprintf("Option<Box<%s>>", t.Owned)
				if declInfo.IsResourceType() {
					t.Param = fmt.Sprintf("Option<%s>", name)
				} else {
					t.Param = fmt.Sprintf("Option<&%s>", name)
				}
			}
		case fidlgen.ProtocolDeclType:
			s := fmt.Sprintf("fidl::endpoints::ClientEnd<%sMarker>", name)
			t.Fidl = fmt.Sprintf("fidl::encoding::Endpoint<%s>", s)
			t.Owned = s
			t.Param = s
			if val.Nullable {
				t.Fidl = fmt.Sprintf("fidl::encoding::Optional<%s>", t.Fidl)
				t.Owned = fmt.Sprintf("Option<%s>", t.Owned)
				t.Param = fmt.Sprintf("Option<%s>", t.Param)
			}
		default:
			panic(fmt.Sprintf("unexpected type: %v", declInfo.Type))
		}
	case fidlgen.InternalType:
		switch val.InternalSubtype {
		case fidlgen.TransportErr:
			s := "fidl::encoding::FrameworkErr"
			t.Fidl = s
			t.Owned = s
			t.Param = s
		default:
			panic(fmt.Sprintf("unknown internal subtype: %v", val.InternalSubtype))
		}
	default:
		panic(fmt.Sprintf("unknown type kind: %v", val.Kind))
	}

	return t
}

// convertParamToEncodeExpr returns an expression that converts a variable v
// from t.Param to a type implementing fidl::encoding::Encode<t.Fidl>.
//
// TODO(fxbug.dev/122199): Remove this once the transition to the new types is
// complete, since parameter types will be encodable as is.
func convertParamToEncodeExpr(v string, t Type) string {
	switch t.Kind {
	case fidlgen.PrimitiveType, fidlgen.StringType, fidlgen.HandleType, fidlgen.RequestType:
		return v
	case fidlgen.ArrayType:
		if t.IsResourceType() {
			return "&mut " + v
		}
		return v
	case fidlgen.VectorType:
		if t.IsResourceType() {
			return v + ".as_mut()"
		}
		return v
	case fidlgen.IdentifierType:
		switch t.DeclType {
		case fidlgen.BitsDeclType, fidlgen.EnumDeclType, fidlgen.ProtocolDeclType:
			return v
		case fidlgen.StructDeclType, fidlgen.TableDeclType, fidlgen.UnionDeclType:
			if t.IsResourceType() {
				if t.Nullable {
					return v + ".as_mut()"
				}
				return "&mut " + v
			}
			return v
		default:
			panic(fmt.Sprintf("unexpected type: %v", t.DeclType))
		}
	default:
		panic(fmt.Sprintf("unknown type kind: %v", t.Kind))
	}
}

// convertResultToEncodeExpr returns an expression that converts a variable v
// from Result<(p.Params[0].Type, p.Params[1].Type, ...), _> to Result<T, _>
// where p is payloadForType(t) and T implements fidl::encoding::Encode<p.FidlType>.
// If len(p.Params) == 1 then the source type is Result<p.Params[0].Type, _>.
func convertResultToEncodeExpr(v string, t Type, p Payload) string {
	switch t.DeclType {
	case fidlgen.StructDeclType:
		var names []string
		var exprs []string
		transform := false
		for _, param := range p.Parameters {
			t := *param.StructMemberType
			expr := convertParamToEncodeExpr(param.Name, t)
			if expr != param.Name {
				transform = true
			}
			if t.IsResourceType() {
				expr = convertMutRefParamToEncodeExpr(param.Name, t)
			} else {
				expr = "*" + expr
			}
			names = append(names, param.Name)
			exprs = append(exprs, expr)
		}
		if transform {
			return fmt.Sprintf("%s.as_mut().map_err(|e| *e).map(|%s| %s)", v, fmtOneOrTuple(names), fmtTuple(exprs))
		}
		if len(names) == 1 {
			return fmt.Sprintf("%s.map(|%s| (%s,))", v, names[0], names[0])
		}
		return v
	case fidlgen.TableDeclType, fidlgen.UnionDeclType:
		if t.IsResourceType() {
			return v + ".as_mut().map_err(|e| *e)"
		}
		return v
	default:
		panic(fmt.Sprintf("unexpected decl type %s", t.DeclType))
	}
}

// convertMutRefParamToEncodeExpr returns an expression that converts a variable
// v from &mut t.Param to a type implementing fidl::encoding::Encode<t.Fidl>.
//
// TODO(fxbug.dev/122199): Remove this once the transition to the new types is
// complete. This is only needed for convertResultToEncodeExpr.
func convertMutRefParamToEncodeExpr(v string, t Type) string {
	switch t.Kind {
	case fidlgen.IdentifierType:
		switch t.DeclType {
		case fidlgen.StructDeclType, fidlgen.TableDeclType, fidlgen.UnionDeclType:
			if t.Nullable {
				if t.IsResourceType() {
					return v + ".as_mut()"
				}
				return v + ".as_ref()"
			}
		}
	}
	return convertMutRefOwnedToEncodeExpr(v, t)
}

// convertMutRefOwnedToEncodeExpr returns an expression that converts a variable
// v from &mut t.Owned to a type implementing fidl::encoding::Encode<t.Fidl>.
//
// TODO(fxbug.dev/122199): Remove this once the transition to the new types is
// complete. This is only needed for convertMutRefResultToEncodeExpr.
func convertMutRefOwnedToEncodeExpr(v string, t Type) string {
	switch t.Kind {
	case fidlgen.PrimitiveType:
		return "*" + v
	case fidlgen.HandleType, fidlgen.RequestType:
		if t.Nullable {
			return fmt.Sprintf("%s.as_mut().map(|x| std::mem::replace(x, fidl::Handle::invalid().into()))", v)
		}
		return fmt.Sprintf("std::mem::replace(%s, fidl::Handle::invalid().into())", v)
	case fidlgen.StringType:
		if t.Nullable {
			return v + ".as_deref()"
		}
		return v + ".as_str()"
	case fidlgen.ArrayType:
		if t.IsResourceType() {
			return v
		}
		return "&*" + v
	case fidlgen.VectorType:
		if t.Nullable {
			if t.IsResourceType() {
				return v + ".as_deref_mut()"
			}
			return v + ".as_deref()"
		}
		if t.IsResourceType() {
			return v + ".as_mut_slice()"
		}
		return v + ".as_slice()"
	case fidlgen.IdentifierType:
		switch t.DeclType {
		case fidlgen.BitsDeclType, fidlgen.EnumDeclType:
			return "*" + v
		case fidlgen.StructDeclType, fidlgen.TableDeclType, fidlgen.UnionDeclType:
			if t.Nullable {
				if t.IsResourceType() {
					return v + ".as_deref_mut()"
				}
				return v + ".as_deref()"
			}
			if t.IsResourceType() {
				return v
			}
			return "&*" + v
		case fidlgen.ProtocolDeclType:
			if t.Nullable {
				return fmt.Sprintf("%s.as_mut().map(|x| std::mem::replace(x, fidl::Handle::invalid().into()))", v)
			}
			return fmt.Sprintf("std::mem::replace(%s, fidl::Handle::invalid().into())", v)
		default:
			panic(fmt.Sprintf("unexpected type: %v", t.DeclType))
		}
	default:
		panic(fmt.Sprintf("unknown type kind: %v", t.Kind))
	}
}

func (c *compiler) compileBits(val fidlgen.Bits) Bits {
	e := Bits{
		Bits:           val,
		Name:           c.compileCamelCompoundIdentifier(val.Name),
		UnderlyingType: c.compileType(val.Type).Owned,
		Members:        []BitsMember{},
	}
	for _, v := range val.Members {
		e.Members = append(e.Members, BitsMember{
			BitsMember: v,
			Name:       compileScreamingSnakeIdentifier(v.Name),
			Value:      c.compileConstant(v.Value, val.Type),
		})
	}
	return e
}

func (c *compiler) compileEnum(val fidlgen.Enum) Enum {
	e := Enum{
		Enum:           val,
		Name:           c.compileCamelCompoundIdentifier(val.Name),
		UnderlyingType: compilePrimitiveSubtype(val.Type),
		Members:        []EnumMember{},
	}
	for _, v := range val.Members {
		e.Members = append(e.Members, EnumMember{
			EnumMember: v,
			Name:       compileCamelIdentifier(v.Name),
			Value:      v.Value.Value,
		})
	}
	e.MinMember = findMinEnumMember(val.Type, e.Members).Name
	return e
}

func findMinEnumMember(typ fidlgen.PrimitiveSubtype, members []EnumMember) EnumMember {
	var res EnumMember
	if typ.IsSigned() {
		min := int64(math.MaxInt64)
		for _, m := range members {
			v, err := strconv.ParseInt(m.Value, 10, 64)
			if err != nil {
				panic(fmt.Sprintf("invalid enum member value: %s", err))
			}
			if v < min {
				min = v
				res = m
			}
		}
	} else {
		min := uint64(math.MaxUint64)
		for _, m := range members {
			v, err := strconv.ParseUint(m.Value, 10, 64)
			if err != nil {
				panic(fmt.Sprintf("invalid enum member value: %s", err))
			}
			if v < min {
				min = v
				res = m
			}
		}
	}
	return res
}

// fmtTuple formats items (types or expressions) as a Rust tuple.
func fmtTuple(items []string) string {
	if len(items) == 0 {
		panic("expected at least one item")
	}
	return fmt.Sprintf("(%s,)", strings.Join(items, ", "))
}

// fmtOneOrTuple is like fmtTuple but does not create 1-tuples.
func fmtOneOrTuple(items []string) string {
	switch len(items) {
	case 0:
		panic("expected at least one item")
	case 1:
		return items[0]
	default:
		return fmt.Sprintf("(%s)", strings.Join(items, ", "))
	}
}

func emptyPayload(fidlType string) Payload {
	return Payload{
		FidlType:        fidlType,
		OwnedType:       "()",
		TupleType:       "()",
		EncodeExpr:      "()",
		ConvertToTuple:  func(owned string) string { return owned },
		ConvertToFields: func(owned string) string { return "" },
	}
}

func (c *compiler) payloadForType(payloadType Type) Payload {
	typeName := c.compileCamelCompoundIdentifier(payloadType.Identifier)
	st, ok := c.structs[payloadType.Identifier]

	// Not a struct: table or union payload.
	if !ok {
		paramName := "payload"
		return Payload{
			FidlType:  typeName,
			OwnedType: typeName,
			TupleType: typeName,
			Parameters: []Parameter{{
				Name:      paramName,
				Type:      payloadType.Param,
				OwnedType: payloadType.Owned,
			}},
			EncodeExpr:      convertParamToEncodeExpr(paramName, payloadType),
			ConvertToTuple:  func(owned string) string { return owned },
			ConvertToFields: func(owned string) string { return fmt.Sprintf("%s: %s,", paramName, owned) },
		}
	}

	// Empty struct with error syntax.
	if len(st.Members) == 0 {
		return emptyPayload("fidl::encoding::EmptyStruct")
	}

	// Struct payload. Flatten it to parameters.
	var parameters []Parameter
	var ownedTypes, encodeExprs []string
	for _, v := range st.Members {
		paramName := compileSnakeIdentifier(v.Name)
		typ := c.compileType(v.Type)
		parameters = append(parameters, Parameter{
			Name:             paramName,
			Type:             typ.Param,
			OwnedType:        typ.Owned,
			StructMemberType: &typ,
		})
		ownedTypes = append(ownedTypes, typ.Owned)
		encodeExprs = append(encodeExprs, convertParamToEncodeExpr(paramName, typ))
	}
	return Payload{
		FidlType:   typeName,
		OwnedType:  typeName,
		TupleType:  fmtOneOrTuple(ownedTypes),
		Parameters: parameters,
		EncodeExpr: fmtTuple(encodeExprs),
		ConvertToTuple: func(owned string) string {
			var exprs []string
			for _, param := range parameters {
				exprs = append(exprs, fmt.Sprintf("%s.%s", owned, param.Name))
			}
			return fmtOneOrTuple(exprs)
		},
		ConvertToFields: func(owned string) string {
			var b strings.Builder
			for _, param := range parameters {
				b.WriteString(fmt.Sprintf("%s: %s.%s,\n", param.Name, owned, param.Name))
			}
			return b.String()
		},
	}
}

func (c *compiler) compileRequest(m fidlgen.Method) Payload {
	if !m.HasRequest {
		// Not applicable (because the method is an event).
		return Payload{}
	}
	if m.RequestPayload == nil {
		// Empty payload, e.g. the request of `Foo();`.
		return emptyPayload("fidl::encoding::EmptyPayload")
	}
	// Struct, table, or union request payload.
	return c.payloadForType(c.compileType(*m.RequestPayload))
}

func (c *compiler) compileResponse(m fidlgen.Method) Payload {
	if !m.HasResponse {
		// Not applicable (because the method is one-way).
		return Payload{}
	}
	if m.ResponsePayload == nil {
		// Empty payload, e.g. the response of `Foo() -> ();` or `-> Foo();`.
		return emptyPayload("fidl::encoding::EmptyPayload")
	}
	if !m.HasResultUnion() {
		// Plain payload with no flexible/error result union.
		return c.payloadForType(c.compileType(*m.ResponsePayload))
	}

	innerType := c.compileType(*m.ValueType)
	inner := c.payloadForType(innerType)
	var errType Type
	if m.HasError {
		errType = c.compileType(*m.ErrorType)
	}

	var p Payload

	// Set FidlType and OwnedType, which are different for each of the 3 cases.
	if m.HasTransportError() && m.HasError {
		p.FidlType = fmt.Sprintf("fidl::encoding::FlexibleResultType<%s, %s>", inner.FidlType, errType.Fidl)
		p.OwnedType = fmt.Sprintf("fidl::encoding::FlexibleResult<%s, %s>", inner.OwnedType, errType.Owned)
	} else if m.HasTransportError() {
		p.FidlType = fmt.Sprintf("fidl::encoding::FlexibleType<%s>", inner.FidlType)
		p.OwnedType = fmt.Sprintf("fidl::encoding::Flexible<%s>", inner.OwnedType)
	} else if m.HasError {
		p.FidlType = fmt.Sprintf("fidl::encoding::ResultType<%s, %s>", inner.FidlType, errType.Fidl)
		p.OwnedType = fmt.Sprintf("Result<%s, %s>", inner.OwnedType, errType.Owned)
	} else {
		panic("should have returned earlier")
	}

	// Set all the other fields, where all that matters is m.HasError.
	if !m.HasError {
		p.TupleType = inner.TupleType
		p.Parameters = inner.Parameters
		p.EncodeExpr = inner.EncodeExpr
		p.ConvertToTuple = inner.ConvertToTuple
		p.ConvertToFields = inner.ConvertToFields
	} else {
		paramName := "result"
		p.TupleType = c.compileCamelCompoundIdentifier(m.ResponsePayload.Identifier)
		p.TupleTypeAliasRhs = fmt.Sprintf("Result<%s, %s>", inner.TupleType, errType.Owned)
		okParamType := "()"
		if len(inner.Parameters) > 0 {
			var paramTypes []string
			for _, param := range inner.Parameters {
				paramTypes = append(paramTypes, param.Type)
			}
			okParamType = fmtOneOrTuple(paramTypes)
		}
		p.Parameters = []Parameter{{
			Name:      paramName,
			Type:      fmt.Sprintf("Result<%s, %s>", okParamType, errType.Param),
			OwnedType: p.TupleType,
		}}
		p.EncodeExpr = convertResultToEncodeExpr(paramName, innerType, inner)
		p.ConvertToTuple = func(owned string) string {
			return fmt.Sprintf("%s.map(|x| %s)", owned, inner.ConvertToTuple("x"))
		}
		p.ConvertToFields = func(owned string) string {
			return fmt.Sprintf("%s: %s.map(|x| %s),", paramName, owned, inner.ConvertToTuple("x"))
		}
	}

	// For FlexibleType and FlexibleResultType, we need to wrap the value before encoding.
	if m.HasTransportError() {
		name, _, _ := strings.Cut(p.OwnedType, "<")
		p.EncodeExpr = fmt.Sprintf("%s::new(%s)", name, p.EncodeExpr)
	}

	return p
}

func (c *compiler) compileProtocol(val fidlgen.Protocol) Protocol {
	r := Protocol{
		Protocol:     val,
		ECI:          val.Name,
		Name:         c.compileCamelCompoundIdentifier(val.Name),
		Methods:      []Method{},
		ProtocolName: strings.Trim(val.GetProtocolName(), "\""),
	}

	for _, v := range val.Methods {
		r.Methods = append(r.Methods, Method{
			Method:    v,
			Name:      compileSnakeIdentifier(v.Name),
			CamelName: compileCamelIdentifier(v.Name),
			Request:   c.compileRequest(v),
			Response:  c.compileResponse(v),
		})
	}

	return r
}

func (c *compiler) compileService(val fidlgen.Service) Service {
	r := Service{
		Service:     val,
		Name:        c.compileCamelCompoundIdentifier(val.Name),
		Members:     []ServiceMember{},
		ServiceName: val.GetServiceName(),
	}

	for _, v := range val.Members {
		m := ServiceMember{
			ServiceMember:     v,
			Name:              string(v.Name),
			CamelName:         compileCamelIdentifier(v.Name),
			SnakeName:         compileSnakeIdentifier(v.Name),
			ProtocolType:      c.compileCamelCompoundIdentifier(v.Type.Identifier),
			ProtocolTransport: v.Type.ProtocolTransport,
		}
		r.Members = append(r.Members, m)
	}

	return r
}

func (c *compiler) compileStructMember(val fidlgen.StructMember) StructMember {
	return StructMember{
		StructMember: val,
		Type:         c.compileType(val.Type),
		Name:         compileSnakeIdentifier(val.Name),
		OffsetV1:     val.FieldShapeV1.Offset,
		OffsetV2:     val.FieldShapeV2.Offset,
	}
}

func (c *compiler) computeUseFidlStructCopyForStruct(st fidlgen.Struct) bool {
	if len(st.Members) == 0 {
		// In Rust, structs containing empty structs do not match the C++ struct layout
		// since empty structs have size 0 in Rust -- even in repr(C).
		return false
	}
	for _, member := range st.Members {
		if !c.computeUseFidlStructCopy(member.Type) {
			return false
		}
	}
	return true
}

func (c *compiler) computeUseFidlStructCopy(typ fidlgen.Type) bool {
	if typ.Nullable {
		return false
	}
	switch typ.Kind {
	case fidlgen.ArrayType:
		return c.computeUseFidlStructCopy(*typ.ElementType)
	case fidlgen.VectorType, fidlgen.StringType, fidlgen.HandleType, fidlgen.RequestType:
		return false
	case fidlgen.PrimitiveType:
		switch typ.PrimitiveSubtype {
		case fidlgen.Bool, fidlgen.Float32, fidlgen.Float64:
			return false
		}
		return true
	case fidlgen.IdentifierType:
		if c.inExternalLibrary(typ.Identifier.Parse()) {
			return false
		}
		declType := c.decls[typ.Identifier].Type
		switch declType {
		case fidlgen.BitsDeclType, fidlgen.EnumDeclType, fidlgen.TableDeclType, fidlgen.UnionDeclType, fidlgen.ProtocolDeclType:
			return false
		case fidlgen.StructDeclType:
			st, ok := c.structs[typ.Identifier]
			if !ok {
				panic(fmt.Sprintf("struct not found: %v", typ.Identifier))
			}
			return c.computeUseFidlStructCopyForStruct(st)
		default:
			panic(fmt.Sprintf("unknown declaration type: %v", declType))
		}
	default:
		panic(fmt.Sprintf("unknown type kind: %v", typ.Kind))
	}
}

func (c *compiler) resolveStruct(identifier fidlgen.EncodedCompoundIdentifier) *fidlgen.Struct {
	if c.inExternalLibrary(identifier.Parse()) {
		// This behavior is matched by computeUseFullStructCopy.
		return nil
	}
	declType := c.decls[identifier].Type
	if declType == fidlgen.StructDeclType {
		st, ok := c.structs[identifier]
		if !ok {
			panic(fmt.Sprintf("struct not found: %v", identifier))
		}
		return &st
	}
	return nil
}

type rustPaddingMarker struct {
	Type   string
	Offset int
	// Mask is a string so it can be in hex.
	Mask string
}

func toRustPaddingMarker(in fidlgen.PaddingMarker) rustPaddingMarker {
	switch len(in.Mask) {
	case 2:
		return rustPaddingMarker{
			Type:   "u16",
			Offset: in.Offset,
			Mask:   fmt.Sprintf("0x%04xu16", binary.LittleEndian.Uint16(in.Mask)),
		}
	case 4:
		return rustPaddingMarker{
			Type:   "u32",
			Offset: in.Offset,
			Mask:   fmt.Sprintf("0x%08xu32", binary.LittleEndian.Uint32(in.Mask)),
		}
	case 8:
		return rustPaddingMarker{
			Type:   "u64",
			Offset: in.Offset,
			Mask:   fmt.Sprintf("0x%016xu64", binary.LittleEndian.Uint64(in.Mask)),
		}
	default:
		panic("unexpected mask size")
	}
}

func toRustPaddingMarkers(in []fidlgen.PaddingMarker) []rustPaddingMarker {
	var out []rustPaddingMarker
	for _, m := range in {
		out = append(out, toRustPaddingMarker(m))
	}
	return out
}

func (c *compiler) compileStruct(val fidlgen.Struct) Struct {
	name := c.compileCamelCompoundIdentifier(val.Name)
	r := Struct{
		Struct:                    val,
		ECI:                       val.Name,
		Name:                      name,
		Members:                   []StructMember{},
		SizeV1:                    val.TypeShapeV1.InlineSize,
		SizeV2:                    val.TypeShapeV2.InlineSize,
		AlignmentV1:               val.TypeShapeV1.Alignment,
		AlignmentV2:               val.TypeShapeV2.Alignment,
		PaddingMarkersV1:          toRustPaddingMarkers(val.BuildPaddingMarkers(fidlgen.WireFormatVersionV1)),
		PaddingMarkersV2:          toRustPaddingMarkers(val.BuildPaddingMarkers(fidlgen.WireFormatVersionV2)),
		FlattenedPaddingMarkersV1: toRustPaddingMarkers(val.BuildFlattenedPaddingMarkers(fidlgen.WireFormatVersionV1, c.resolveStruct)),
		FlattenedPaddingMarkersV2: toRustPaddingMarkers(val.BuildFlattenedPaddingMarkers(fidlgen.WireFormatVersionV2, c.resolveStruct)),
	}

	for _, v := range val.Members {
		member := c.compileStructMember(v)
		r.Members = append(r.Members, member)
		r.HasPadding = r.HasPadding || (v.FieldShapeV1.Padding != 0)
	}

	r.UseFidlStructCopy = c.computeUseFidlStructCopyForStruct(val)

	return r
}

func (c *compiler) compileUnion(val fidlgen.Union) Union {
	r := Union{
		Union: val,
		ECI:   val.Name,
		Name:  c.compileCamelCompoundIdentifier(val.Name),
	}
	for _, v := range val.Members {
		if v.Reserved {
			continue
		}
		r.Members = append(r.Members, UnionMember{
			UnionMember: v,
			Type:        c.compileType(*v.Type),
			Name:        compileCamelIdentifier(v.Name),
			Ordinal:     v.Ordinal,
		})
	}
	return r
}

func (c *compiler) compileTable(table fidlgen.Table) Table {
	r := Table{
		Table: table,
		ECI:   table.Name,
		Name:  c.compileCamelCompoundIdentifier(table.Name),
	}
	for _, member := range table.SortedMembersNoReserved() {
		r.Members = append(r.Members, TableMember{
			TableMember: member,
			Type:        c.compileType(*member.Type),
			Name:        compileSnakeIdentifier(member.Name),
			Ordinal:     member.Ordinal,
		})
	}
	return r
}

type derives uint16

const (
	derivesDebug derives = 1 << iota
	derivesCopy
	derivesClone
	derivesDefault
	derivesEq
	derivesPartialEq
	derivesOrd
	derivesPartialOrd
	derivesHash
	derivesAll derives = (1 << iota) - 1
)

// note: keep this list in the same order as the derives definitions
var derivesNames = []string{
	// [START derived_traits]
	"Debug",
	"Copy",
	"Clone",
	"Default",
	"Eq",
	"PartialEq",
	"Ord",
	"PartialOrd",
	"Hash",
	// [END derived_traits]
}

// Returns the derives that are allowed for a type based on resourceness
func allowedDerives(r fidlgen.Resourceness) derives {
	if r.IsResourceType() {
		return derivesAll &^ (derivesCopy | derivesClone)
	}
	return derivesAll
}

// Returns the minimal derives we can assume a type has based on resourceness.
func minimalDerives(r fidlgen.Resourceness) derives {
	if r.IsValueType() {
		return derivesDebug | derivesPartialEq | derivesClone
	}
	return derivesDebug | derivesPartialEq
}

// RemoveCustom removes traits by name. Templates use it when they are providing
// a custom impl instead of deriving. (We can't just omit the traits in
// fillDerives because it would recursively remove it from containing types.)
func (v derives) RemoveCustom(traits ...string) (derives, error) {
	result := v
	for _, name := range traits {
		found := false
		for i, n := range derivesNames {
			if n == name {
				found = true
				result = result &^ (1 << i)
				break
			}
		}
		if !found {
			return 0, fmt.Errorf("trait '%s' not found", name)
		}
	}
	return result, nil
}

func (v derives) String() string {
	var parts []string
	for i, bit := 0, derives(1); bit&derivesAll != 0; i, bit = i+1, bit<<1 {
		if v&bit != 0 {
			parts = append(parts, derivesNames[i])
		}
	}
	if len(parts) == 0 {
		return ""
	}
	sort.Strings(parts)
	return fmt.Sprintf("#[derive(%s)]", strings.Join(parts, ", "))
}

// The status of derive calculation for a particular type.
type deriveStatus struct {
	// recursing indicates whether or not we've passed through this type already
	// on a recursive descent. This is used to prevent unbounded recursion on
	// mutually-recursive types.
	recursing bool
	// complete indicates whether or not the derive for the given type has
	// already been successfully calculated and stored in the IR.
	complete bool
}

// The state of the current calculation of derives.
type derivesCompiler struct {
	*compiler
	topMostCall                bool
	didShortCircuitOnRecursion bool
	statuses                   map[EncodedCompoundIdentifier]deriveStatus
	root                       *Root
}

// [START fill_derives]
// Calculates what traits should be derived for each output type,
// filling in all `*derives` in the IR.
func (c *compiler) fillDerives(ir *Root) {
	// [END fill_derives]
	dc := &derivesCompiler{
		compiler:                   c,
		topMostCall:                true,
		didShortCircuitOnRecursion: false,
		statuses:                   make(map[EncodedCompoundIdentifier]deriveStatus),
		root:                       ir,
	}

	// Bits derives are controlled by the bitflags crate.
	// Enums derive a constant set of traits hardcoded in enum.tmpl.
	for _, v := range ir.Structs {
		dc.fillDerivesForECI(v.ECI)
	}
	for _, v := range ir.Unions {
		dc.fillDerivesForECI(v.ECI)
	}
	for _, v := range ir.Tables {
		dc.fillDerivesForECI(v.ECI)
	}
}

// Computes derives for a struct, table, or union.
// Also fills in the .Derives field if the type is local to this library.
func (dc *derivesCompiler) fillDerivesForECI(eci EncodedCompoundIdentifier) derives {
	declInfo, ok := dc.decls[eci]
	if !ok {
		panic(fmt.Sprintf("declaration not found: %v", eci))
	}

	// TODO(fxbug.dev/61760): Make external type information available here.
	// Currently, we conservatively assume external structs, tables, and unions
	// only derive a minimal set of traits, which includes Clone for value types
	// (not having Clone is especially annoying, so we put resourceness of
	// external types into the IR as a stopgap solution).
	if dc.inExternalLibrary(eci.Parse()) {
		return minimalDerives(*declInfo.Resourceness)
	}

	topMostCall := dc.topMostCall
	if dc.topMostCall {
		dc.topMostCall = false
	}
	deriveStatus := dc.statuses[eci]
	if deriveStatus.recursing {
		// If we've already seen the current type while recursing,
		// the algorithm has already explored all of the other fields contained
		// within the cycle, so we can return true for all derives, and the
		// correct results will be bubbled up.
		dc.didShortCircuitOnRecursion = true
		return derivesAll
	}
	deriveStatus.recursing = true
	dc.statuses[eci] = deriveStatus

	var derivesOut derives
	switch declInfo.Type {
	case fidlgen.StructDeclType:
		st := dc.root.findStruct(eci)
		if st == nil {
			panic(fmt.Sprintf("struct not found: %v", eci))
		}
		if deriveStatus.complete {
			derivesOut = st.Derives
			break
		}
		derivesOut = allowedDerives(st.Resourceness)
		for _, member := range st.Members {
			derivesOut &= dc.derivesForType(member.Type)
		}
		derivesOut &^= derivesDefault
		st.Derives = derivesOut
	case fidlgen.TableDeclType:
		table := dc.root.findTable(eci)
		if table == nil {
			panic(fmt.Sprintf("table not found: %v", eci))
		}
		if deriveStatus.complete {
			derivesOut = table.Derives
			break
		}
		derivesOut = minimalDerives(*declInfo.Resourceness)
		// We can always derive Default because table fields are optional.
		derivesOut |= derivesDefault
		table.Derives = derivesOut
	case fidlgen.UnionDeclType:
		union := dc.root.findUnion(eci)
		if union == nil {
			panic(fmt.Sprintf("union not found: %v", eci))
		}
		if deriveStatus.complete {
			derivesOut = union.Derives
			break
		}
		if union.IsFlexible() {
			derivesOut = minimalDerives(union.Resourceness)
		} else {
			derivesOut = allowedDerives(union.Resourceness)
			for _, member := range union.Members {
				derivesOut &= dc.derivesForType(member.Type)
			}
		}
		derivesOut &^= derivesDefault
		union.Derives = derivesOut
	default:
		panic(fmt.Sprintf("unexpected declaration type: %v", declInfo.Type))
	}
	if topMostCall || !dc.didShortCircuitOnRecursion {
		// Our completed result is only valid if it's either at top-level
		// (ensuring we've fully visited all child types in the recursive
		// substructure at least once) or if we performed no recursion-based
		// short-circuiting, in which case results are correct and absolute.
		//
		// Note that non-topMostCalls are invalid if we did short-circuit
		// on recursion, because we might have short-circuited just beneath
		// a type without having explored all of its children at least once
		// beneath it.
		//
		// For example, imagine A -> B -> C -> A.
		// When we start on A, we go to B, then go to C, then go to A, at which
		// point we short-circuit. The intermediate result for C is invalid
		// for anything except the computation of A and B above, as it does
		// not take into account that C contains A and B, only that it contains
		// its top-level fields (other than A). In order to get a correct
		// idea of the shape of C, we have to start with C, following through
		// every branch until we find C again.
		deriveStatus.complete = true
	}
	if topMostCall {
		// Reset intermediate state
		dc.topMostCall = true
		dc.didShortCircuitOnRecursion = false
	}
	deriveStatus.recursing = false
	dc.statuses[eci] = deriveStatus
	return derivesOut
}

func (dc *derivesCompiler) derivesForType(t Type) derives {
	switch t.Kind {
	case fidlgen.ArrayType:
		return dc.derivesForType(*t.ElementType)
	case fidlgen.VectorType:
		return derivesAll &^ derivesCopy & dc.derivesForType(*t.ElementType)
	case fidlgen.StringType:
		return derivesAll &^ derivesCopy
	case fidlgen.HandleType, fidlgen.RequestType:
		return derivesAll &^ (derivesCopy | derivesClone)
	case fidlgen.PrimitiveType:
		switch t.PrimitiveSubtype {
		case fidlgen.Bool:
			return derivesAll
		case fidlgen.Int8, fidlgen.Int16, fidlgen.Int32, fidlgen.Int64,
			fidlgen.Uint8, fidlgen.Uint16, fidlgen.Uint32, fidlgen.Uint64:
			return derivesAll
		case fidlgen.Float32, fidlgen.Float64:
			// Floats don't have a total ordering due to NAN and its multiple representations.
			return derivesAll &^ (derivesEq | derivesOrd | derivesHash)
		default:
			panic(fmt.Sprintf("unknown primitive type: %v", t.PrimitiveSubtype))
		}
	case fidlgen.IdentifierType:
		switch t.DeclType {
		case fidlgen.BitsDeclType, fidlgen.EnumDeclType:
			return derivesAll
		case fidlgen.StructDeclType, fidlgen.UnionDeclType:
			result := dc.fillDerivesForECI(t.Identifier)
			if t.Nullable {
				// Nullable structs and unions gets put in Option<Box<...>>.
				result &^= derivesCopy
			}
			return result
		case fidlgen.TableDeclType:
			return dc.fillDerivesForECI(t.Identifier)
		case fidlgen.ProtocolDeclType:
			// An IdentifierType referring to a Protocol is a client_end.
			return derivesAll &^ (derivesCopy | derivesClone)
		default:
			panic(fmt.Sprintf("unexpected identifier type: %v", t.DeclType))
		}
	default:
		panic(fmt.Sprintf("unknown type kind: %v", t.Kind))
	}
}

func Compile(r fidlgen.Root) Root {
	r = r.ForBindings("rust")
	r = r.ForTransport("Channel")
	root := Root{
		Experiments: r.Experiments,
	}
	thisLibParsed := r.Name.Parse()
	c := compiler{
		decls:        r.DeclInfo(),
		experiments:  r.Experiments,
		library:      thisLibParsed,
		externCrates: map[string]struct{}{},
		structs:      map[fidlgen.EncodedCompoundIdentifier]fidlgen.Struct{},
	}

	methodUsage := r.MethodTypeUsageMap()

	for _, s := range r.Structs {
		c.structs[s.Name] = s
	}

	for _, s := range r.ExternalStructs {
		c.structs[s.Name] = s
	}

	for _, v := range r.Bits {
		root.Bits = append(root.Bits, c.compileBits(v))
	}

	for _, v := range r.Consts {
		root.Consts = append(root.Consts, c.compileConst(v))
	}

	for _, v := range r.Enums {
		root.Enums = append(root.Enums, c.compileEnum(v))
	}

	for _, v := range r.Services {
		root.Services = append(root.Services, c.compileService(v))
	}

	for _, v := range r.Tables {
		root.Tables = append(root.Tables, c.compileTable(v))
	}

	for _, v := range r.Structs {
		// Don't emit compiler-generated "TopResponse" structs.
		if methodUsage[v.Name] == fidlgen.UsedOnlyAsMessageBody {
			continue
		}
		// Don't emit compiler-generated empty structs for error syntax.
		if methodUsage[v.Name] == fidlgen.UsedOnlyAsPayload && len(v.Members) == 0 {
			continue
		}
		root.Structs = append(root.Structs, c.compileStruct(v))
	}

	for _, v := range r.Unions {
		// Don't emit compiler-generated "Result" unions.
		if v.HasAttribute("result") {
			continue
		}
		root.Unions = append(root.Unions, c.compileUnion(v))
	}

	for _, v := range r.Protocols {
		root.Protocols = append(root.Protocols, c.compileProtocol(v))
	}

	c.fillDerives(&root)

	thisLibCompiled := compileLibraryName(thisLibParsed)

	// Sort the extern crates to make sure the generated file is
	// consistent across builds.
	var externCrates []string
	for k := range c.externCrates {
		externCrates = append(externCrates, k)
	}
	sort.Strings(externCrates)

	for _, k := range externCrates {
		if k != thisLibCompiled {
			root.ExternCrates = append(root.ExternCrates, k)
		}
	}

	return root
}
