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

type PythonConst struct {
	fidlgen.Const
	PythonName  string
	PythonValue string
	PythonType  PythonType
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
	Library           string
	PythonName        string
	PythonMembers     []PythonUnionMember
	PythonSuccessType *PythonType
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

type UnsupportedMessage string

type PythonUnsupported struct {
	Identifier fidlgen.EncodedCompoundIdentifier
	PythonName string
	Message    UnsupportedMessage
}

type PythonProtocol struct {
	fidlgen.Protocol
	Discoverable           bool
	Marker                 string
	Library                string
	PythonName             string
	PythonMarkerName       string
	PythonClientName       string
	PythonServerName       string
	PythonEventHandlerName string
}

type PythonRequest struct {
	DeclType         fidlgen.DeclType
	PythonType       PythonType
	PythonParameters []PythonParameter
}

type PythonParameter struct {
	PythonType    PythonType
	PythonName    string
	PythonDefault *string
}

type PythonRoot struct {
	fidlgen.Root
	PythonModuleName       string
	PythonTables           []PythonTable
	PythonStructs          []PythonStruct
	ExternalPythonStructs  []PythonStruct
	PythonUnions           []PythonUnion
	PythonAliases          []PythonAlias
	PythonConsts           []PythonConst
	PythonBits             []PythonBits
	PythonEnums            []PythonEnum
	PythonProtocols        []PythonProtocol
	PythonExternalModules  []string
	PythonUnsupportedTypes []PythonUnsupported
}

type compiler struct {
	decls               fidlgen.DeclInfoMap
	library             fidlgen.EncodedLibraryIdentifier
	externalModules     map[string]struct{}
	EmptySuccessStructs map[fidlgen.EncodedCompoundIdentifier]fidlgen.Struct
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

	var name string
	if c.lookupDeclInfo(val).Type == fidlgen.ConstDeclType {
		name = compileScreamingSnakeIdentifier(ci.Name)
	} else {
		name = compileCamelIdentifier(ci.Name)
	}

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
	if _, ok := c.EmptySuccessStructs[val.Identifier]; ok {
		return &PythonType{
			Type:       val,
			PythonName: "None",
		}
	}

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
	case fidlgen.EndpointType:
		switch val.Role {
		case fidlgen.ClientRole, fidlgen.ServerRole:
			name += "int"
		default:
			log.Fatalf("Unsupported endpoint role: %v", val)
		}
	case fidlgen.InternalType:
		// TODO(https://fxbug.dev/42061151): Remove "transport_error".
		switch val.InternalSubtype {
		case "framework_error", "transport_error":
			name += "fidl.FrameworkError"
		default:
			log.Fatalf("Unrecognized internal type: %v", val)
		}
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
	var r string
	switch val.Kind {
	case fidlgen.NumericLiteral:
		r = val.Value
	case fidlgen.BoolLiteral:
		if val.Value == "true" {
			r = "True"
			return &r
		} else if val.Value == "false" {
			r = "False"
		} else {
			log.Fatalf("Unknown bool value: %v", val)
		}
	case fidlgen.StringLiteral:
		r = fmt.Sprintf("\"%s\"", val.Value)
	default:
		log.Fatalf("Unknown literal kind: %v", val)
	}
	return &r
}

func (c *compiler) compileMemberIdentifier(val fidlgen.EncodedCompoundIdentifier) *string {
	ci := val.Parse()
	if ci.Member == "" {
		log.Fatalf("expected a member: %s", val)
	}
	decl := val.DeclName()
	declType := c.lookupDeclInfo(decl).Type
	var member string
	switch declType {
	case fidlgen.BitsDeclType, fidlgen.EnumDeclType:
		member = compileScreamingSnakeIdentifier(ci.Member)
	default:
		log.Fatalf("unexpected decl type: %s", declType)
	}
	member_identifier := fmt.Sprintf("%s.%s", *c.compileDeclIdentifier(decl), member)
	return &member_identifier
}

func (c *compiler) compileConstant(val fidlgen.Constant, typ fidlgen.Type) *string {
	switch val.Kind {
	case fidlgen.LiteralConstant:
		return c.compileLiteral(*val.Literal)
	case fidlgen.BinaryOperator, fidlgen.IdentifierConstant:
		return &val.Value
	}
	log.Fatalf("Failed to compile constant: %v, %v", val, typ)
	return nil
}

func (c *compiler) compileConst(val fidlgen.Const) PythonConst {
	return PythonConst{
		Const:       val,
		PythonName:  *c.compileDeclIdentifier(val.Name),
		PythonValue: *c.compileConstant(val.Value, val.Type),
		PythonType:  *c.compileType(val.Type, nil),
	}
}

func (c *compiler) compileBits(val fidlgen.Bits) PythonBits {
	e := PythonBits{
		Bits:          val,
		PythonName:    *c.compileDeclIdentifier(val.Name),
		PythonMembers: []PythonBitsMember{},
		Empty:         len(val.Members) == 0,
	}
	for _, member_val := range val.Members {
		e.PythonMembers = append(e.PythonMembers, PythonBitsMember{
			BitsMember:  member_val,
			PythonName:  changeIfReserved(compileScreamingSnakeIdentifier(member_val.Name)),
			PythonValue: *c.compileConstant(member_val.Value, val.Type),
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
		case fidlgen.BinaryOperator, fidlgen.IdentifierConstant:
			value = member_val.Value.Value
		default:
			log.Fatalf("Unknown enum member kind: %v", member_val)
		}

		e.PythonMembers = append(e.PythonMembers, PythonEnumMember{
			EnumMember:  member_val,
			PythonName:  changeIfReserved(compileScreamingSnakeIdentifier(member_val.Name)),
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
		PythonName:  changeIfReserved(compileSnakeIdentifier(val.Name)),
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

func (c *compiler) compileStructMember(val fidlgen.StructMember) (PythonStructMember, *UnsupportedMessage) {
	if _, ok := val.Attributes.LookupAttribute("allow_deprecated_struct_defaults"); ok {
		message := UnsupportedMessage(fmt.Sprintf("%s annotated with allow_deprecated_struct_defaults", val.Name))
		return PythonStructMember{}, &message
	}
	t := c.compileType(val.Type, val.MaybeFromAlias)
	if t == nil {
		message := UnsupportedMessage(fmt.Sprintf("Failed to compile type of %s", val.Name))
		return PythonStructMember{}, &message
	}
	return PythonStructMember{
		StructMember: val,
		PythonType:   *t,
		PythonName:   changeIfReserved(compileSnakeIdentifier(val.Name)),
	}, nil
}

func (c *compiler) compileStruct(val fidlgen.Struct) (PythonStruct, *PythonUnsupported) {
	name := *c.compileDeclIdentifier(val.Name)
	python_struct := PythonStruct{
		Struct:        val,
		Library:       string(val.Name.LibraryName()),
		PythonName:    name,
		PythonMembers: []PythonStructMember{},
	}

	unsupported_message := ""
	for _, v := range val.Members {
		member, message := c.compileStructMember(v)
		if message != nil {
			unsupported_message = fmt.Sprintf("%s\n    - %s", unsupported_message, *message)
			continue
		}
		python_struct.PythonMembers = append(python_struct.PythonMembers, member)
	}
	if unsupported_message != "" {
		unsupported := PythonUnsupported{
			Identifier: val.Name,
			PythonName: name,
			Message:    UnsupportedMessage("\n" + unsupported_message),
		}
		return PythonStruct{}, &unsupported
	}

	return python_struct, nil
}

var pythonReservedWords = map[string]struct{}{
	// LINT.IfChange
	// keep-sorted start
	"ArithmeticError":           {}, //
	"AssertionError":            {}, //
	"AttributeError":            {}, //
	"BaseException":             {}, //
	"BaseExceptionGroup":        {}, //
	"BlockingIOError":           {}, //
	"BrokenPipeError":           {}, //
	"BufferError":               {}, //
	"BytesWarning":              {}, //
	"ChildProcessError":         {}, //
	"ConnectionAbortedError":    {}, //
	"ConnectionError":           {}, //
	"ConnectionRefusedError":    {}, //
	"ConnectionResetError":      {}, //
	"DeprecationWarning":        {}, //
	"EOFError":                  {}, //
	"Ellipsis":                  {}, //
	"EncodingWarning":           {}, //
	"EnvironmentError":          {}, //
	"Exception":                 {}, //
	"ExceptionGroup":            {}, //
	"False":                     {}, //
	"FileExistsError":           {}, //
	"FileNotFoundError":         {}, //
	"FloatingPointError":        {}, //
	"FutureWarning":             {}, //
	"GeneratorExit":             {}, //
	"IOError":                   {}, //
	"ImportError":               {}, //
	"ImportWarning":             {}, //
	"IndentationError":          {}, //
	"IndexError":                {}, //
	"InterruptedError":          {}, //
	"IsADirectoryError":         {}, //
	"KeyError":                  {}, //
	"KeyboardInterrupt":         {}, //
	"LookupError":               {}, //
	"MemoryError":               {}, //
	"ModuleNotFoundError":       {}, //
	"NameError":                 {}, //
	"None":                      {}, //
	"NotADirectoryError":        {}, //
	"NotImplemented":            {}, //
	"NotImplementedError":       {}, //
	"OSError":                   {}, //
	"OverflowError":             {}, //
	"PendingDeprecationWarning": {}, //
	"PermissionError":           {}, //
	"ProcessLookupError":        {}, //
	"RecursionError":            {}, //
	"ReferenceError":            {}, //
	"ResourceWarning":           {}, //
	"RuntimeError":              {}, //
	"RuntimeWarning":            {}, //
	"StopAsyncIteration":        {}, //
	"StopIteration":             {}, //
	"SyntaxError":               {}, //
	"SyntaxWarning":             {}, //
	"SystemError":               {}, //
	"SystemExit":                {}, //
	"TabError":                  {}, //
	"TimeoutError":              {}, //
	"True":                      {}, //
	"TypeError":                 {}, //
	"UnboundLocalError":         {}, //
	"UnicodeDecodeError":        {}, //
	"UnicodeEncodeError":        {}, //
	"UnicodeError":              {}, //
	"UnicodeTranslateError":     {}, //
	"UnicodeWarning":            {}, //
	"UserWarning":               {}, //
	"ValueError":                {}, //
	"Warning":                   {}, //
	"ZeroDivisionError":         {}, //
	"abs":                       {}, //
	"aiter":                     {}, //
	"all":                       {}, //
	"and":                       {}, //
	"anext":                     {}, //
	"any":                       {}, //
	"as":                        {}, //
	"ascii":                     {}, //
	"assert":                    {}, //
	"async":                     {}, //
	"await":                     {}, //
	"bin":                       {}, //
	"bool":                      {}, //
	"break":                     {}, //
	"breakpoint":                {}, //
	"bytearray":                 {}, //
	"bytes":                     {}, //
	"callable":                  {}, //
	"case":                      {}, //
	"chr":                       {}, //
	"class":                     {}, //
	"classmethod":               {}, //
	"compile":                   {}, //
	"complex":                   {}, //
	"continue":                  {}, //
	"copyright":                 {}, //
	"credits":                   {}, //
	"def":                       {}, //
	"del":                       {}, //
	"delattr":                   {}, //
	"dict":                      {}, //
	"dir":                       {}, //
	"divmod":                    {}, //
	"elif":                      {}, //
	"else":                      {}, //
	"enumerate":                 {}, //
	"eval":                      {}, //
	"except":                    {}, //
	"exec":                      {}, //
	"exit":                      {}, //
	"filter":                    {}, //
	"finally":                   {}, //
	"float":                     {}, //
	"for":                       {}, //
	"format":                    {}, //
	"from":                      {}, //
	"frozenset":                 {}, //
	"getattr":                   {}, //
	"global":                    {}, //
	"globals":                   {}, //
	"hasattr":                   {}, //
	"hash":                      {}, //
	"help":                      {}, //
	"hex":                       {}, //
	"id":                        {}, //
	"if":                        {}, //
	"import":                    {}, //
	"in":                        {}, //
	"input":                     {}, //
	"int":                       {}, //
	"is":                        {}, //
	"isinstance":                {}, //
	"issubclass":                {}, //
	"iter":                      {}, //
	"lambda":                    {}, //
	"len":                       {}, //
	"license":                   {}, //
	"list":                      {}, //
	"locals":                    {}, //
	"map":                       {}, //
	"match":                     {}, //
	"max":                       {}, //
	"memoryview":                {}, //
	"min":                       {}, //
	"next":                      {}, //
	"nonlocal":                  {}, //
	"not":                       {}, //
	"object":                    {}, //
	"oct":                       {}, //
	"open":                      {}, //
	"or":                        {}, //
	"ord":                       {}, //
	"pass":                      {}, //
	"pow":                       {}, //
	"print":                     {}, //
	"property":                  {}, //
	"quit":                      {}, //
	"raise":                     {}, //
	"range":                     {}, //
	"repr":                      {}, //
	"return":                    {}, //
	"reversed":                  {}, //
	"round":                     {}, //
	"self":                      {}, //
	"set":                       {}, //
	"setattr":                   {}, //
	"slice":                     {}, //
	"sorted":                    {}, //
	"staticmethod":              {}, //
	"str":                       {}, //
	"sum":                       {}, //
	"super":                     {}, //
	"try":                       {}, //
	"tuple":                     {}, //
	"type":                      {}, //
	"vars":                      {}, //
	"while":                     {}, //
	"with":                      {}, //
	"yield":                     {}, //
	"zip":                       {}, //
	// keep-sorted end
	// LINT.ThenChange(//src/developer/ffx/lib/fuchsia-controller/cpp/fidl_codec/utils.h, //src/developer/ffx/lib/fuchsia-controller/python/fidl/_library.py, //tools/fidl/fidlgen_python/codegen/ir.go, //tools/fidl/gidl/backend/fuchsia_controller/conformance.go)
}

func changeIfReserved(s string) string {
	if _, ok := pythonReservedWords[s]; ok {
		return s + "_"
	}
	return s
}

func (c *compiler) compileUnionMember(val fidlgen.UnionMember) PythonUnionMember {
	t := c.compileType(val.Type, val.MaybeFromAlias)
	if t == nil {
		log.Fatalf("Type not supported")
	}
	return PythonUnionMember{
		UnionMember: val,
		PythonType:  *t,
		PythonName:  changeIfReserved(compileSnakeIdentifier(val.Name)),
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
		if python_union.IsResult && member.PythonName == "response" {
			python_union.PythonSuccessType = &member.PythonType
		}
	}

	return python_union
}

func (c *compiler) compileRequest(m fidlgen.Method) *PythonRequest {
	return nil
}

func (c *compiler) compileProtocol(val fidlgen.Protocol) PythonProtocol {
	name := *c.compileDeclIdentifier(val.Name)
	r := PythonProtocol{
		Protocol:               val,
		PythonName:             name,
		Library:                string(c.library),
		PythonMarkerName:       name + "Marker",
		PythonClientName:       name + "Client",
		PythonServerName:       name + "Server",
		PythonEventHandlerName: name + "EventHandler",
	}

	// Compile the marker for discovering a protocol, if it's discoverable,
	// e.g. "fuchsia.developer.ffx.Echo".
	if _, r.Discoverable = val.LookupAttribute("discoverable"); r.Discoverable {
		r.Marker = strings.Trim(val.GetProtocolName(), "\"")
	} else {
		r.Marker = fmt.Sprintf("(nondiscoverable) %s", val.Name)
	}

	return r
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
		decls:               root.DeclInfo(),
		library:             root.Name,
		externalModules:     map[string]struct{}{},
		EmptySuccessStructs: map[fidlgen.EncodedCompoundIdentifier]fidlgen.Struct{},
	}

	// Collect all empty success structs so that all calls to compileType() will return None as
	// their type.
	for _, v := range root.Structs {
		if v.IsEmptySuccessStruct {
			c.EmptySuccessStructs[v.Name] = v
		}
	}
	// The only known uses of ExternalStructs are from fuchsia.unknown which provides some empty
	// success structs. Otherwise, it seems safe to ignore types in ExternalStructs.
	for _, v := range root.ExternalStructs {
		if v.IsEmptySuccessStruct {
			c.EmptySuccessStructs[v.Name] = v
		}
		python_struct, python_unsupported := c.compileStruct(v)
		if python_unsupported != nil {
			python_root.PythonUnsupportedTypes = append(python_root.PythonUnsupportedTypes, *python_unsupported)
		} else {
			python_root.ExternalPythonStructs = append(python_root.ExternalPythonStructs, python_struct)
		}
	}

	for _, v := range root.Tables {
		python_root.PythonTables = append(python_root.PythonTables, c.compileTable(v))
	}

	for _, v := range root.Structs {
		if v.IsEmptySuccessStruct {
			c.EmptySuccessStructs[v.Name] = v
		}
		python_struct, python_unsupported := c.compileStruct(v)
		if python_unsupported != nil {
			python_root.PythonUnsupportedTypes = append(python_root.PythonUnsupportedTypes, *python_unsupported)
		} else {
			python_root.PythonStructs = append(python_root.PythonStructs, python_struct)
		}
	}

	for _, v := range root.Aliases {
		python_root.PythonAliases = append(python_root.PythonAliases, c.compileAlias(v))
	}

	// TODO(https://fxbug.dev/396552135): Extend the test suite to cover the different variations
	// of bits declarations.
	for _, v := range root.Bits {
		python_root.PythonBits = append(python_root.PythonBits, c.compileBits(v))
	}

	for _, v := range root.Consts {
		python_root.PythonConsts = append(python_root.PythonConsts, c.compileConst(v))
	}

	for _, v := range root.Enums {
		python_root.PythonEnums = append(python_root.PythonEnums, c.compileEnum(v))
	}

	for _, v := range root.Unions {
		// TODO(https://fxbug.dev/394421154): If v is a result, then its creation should
		// be deferred to when the corresponding protocol method is compiled.
		python_root.PythonUnions = append(python_root.PythonUnions, c.compileUnion(v))
	}

	for _, v := range root.Protocols {
		python_root.PythonProtocols = append(python_root.PythonProtocols, c.compileProtocol(v))
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
