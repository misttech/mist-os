// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Note: The names of the structs in this package match the corresponding clang-doc class names.
package clangdoc

import (
	"archive/zip"
	"io"
	"log"
	"os"
	"path"
	"strings"

	"gopkg.in/yaml.v2"
)

// Debug can be set to true to add more logging information.
// Or the logging could be migrated to use glog, and make use of Google logging
// flags.
var Debug bool

type Location struct {
	LineNumber int    `yaml:"LineNumber"`
	Filename   string `yaml:"Filename"`
}

// PathNameToFullyQualified converts a Path + Name that is used for types and references into a
// fully-qualified C++ name.
//
// TODO(https://fxbug.dev/119082): This will skip template parameters which are not encoded in the path.
// Implementing this properly will require looking up each USR in the index and getting the template
// parameters. If you can get a QualName (on a Reference) it will contain the template parameters
// and should be used instead.
func PathNameToFullyQualified(path, name string) string {
	// The Path uses slash separators.
	if len(path) == 0 {
		// No scoping.
		return name
	}
	if path == "GlobalNamespace" {
		// Clang-doc generates a "GlobalNamespace" at the toplevel.
		return name
	}

	// Replace "std::__2" prefixes which are standard library versioning stuff that the user
	// doesn't want to see.
	if strings.HasPrefix(path, "std/__2") {
		path = strings.Replace(path, "std/__2", "std", 1)
	}

	return strings.ReplaceAll(path, "/", "::") + "::" + name
}

type Reference struct {
	Type                string `yaml:"Type"` // e.g. "Namespace", "Record".
	Name                string `yaml:"Name"`
	QualName            string `yaml:"QualName"`
	USR                 string `yaml:"USR"`
	Path                string `yaml:"Path"`
	IsInGlobalNamespace bool   `yaml:"IsInGlobalNamespace"`
}

// Makes a simple reference with no USR (useful for tests).
func MakeReference(name string, qualName string, path string) Reference {
	return Reference{
		Name:                name,
		QualName:            qualName,
		Path:                path,
		IsInGlobalNamespace: len(path) == 0,
	}
}
func MakeGlobalReference(name string) Reference {
	return MakeReference(name, name, "")
}

type Type struct {
	Reference Reference `yaml:"Type"`
}

func MakeType(name string, qualName string, path string) Type {
	return Type{Reference: MakeReference(name, qualName, path)}
}

func MakeGlobalType(name string) Type {
	return MakeType(name, name, "")
}

type CommentInfo struct {
	Kind        string        `yaml:"Kind"`
	Text        string        `yaml:"Text"`
	Name        string        `yaml:"Name"`
	Direction   string        `yaml:"Direction"` // in/out for parameter comments
	ParamName   string        `yaml:"ParamName"`
	CloseName   string        `yaml:"CloseName"`
	SelfClosing bool          `yaml:"SelfClosing"`
	Explicit    bool          `yaml:"Explicit"`
	Args        string        `yaml:"Args"`
	AttrKeys    []string      `yaml:"AttrKeys"`
	AttrValues  []string      `yaml:"AttrValues"`
	Children    []CommentInfo `yaml:"Children"`
}

type TemplateParamInfo struct {
	Contents string `yaml:"Contents"`
}

type TemplateSpecializationInfo struct {
	SpecializationOf string              `yaml:"SpecializationOf"`
	Params           []TemplateParamInfo `yaml:"Params"`
}

type TemplateInfo struct {
	Params []TemplateParamInfo `yaml:"Params"`

	// Will be null if this is not a specialization.
	Specialization *TemplateSpecializationInfo `yaml:"Specialization"`
}

// FieldTypeInfo is a field with a name and a type. It is used for function parameters. See also
// MemberTypeInfo which adds an access tag (basically inheritance).
type FieldTypeInfo struct {
	Name         string    `yaml:"Name"`
	TypeRef      Reference `yaml:"Type"`
	DefaultValue string    `yaml:"DefaultValue"`
}

type MemberTypeInfo struct {
	Name         string        `yaml:"Name"`
	TypeRef      Reference     `yaml:"Type"`
	FullTypeName string        `yaml:"FullTypeName"`
	Access       string        `yaml:"Access"`
	Description  []CommentInfo `yaml:"Description"`
}

func (m MemberTypeInfo) IsPublic() bool {
	return m.Access == "Public"
}
func (m MemberTypeInfo) IsPrivate() bool {
	// Clang-doc seems to omit access for private members.
	return len(m.Access) == 0 || m.Access == "Private"
}
func (m MemberTypeInfo) IsProtected() bool {
	return m.Access == "Protected"
}

type FunctionInfo struct {
	USR  string `yaml:"USR"`
	Name string `yaml:"Name"`

	// See RecordInfo.Namespace for documentation.
	Namespace []Reference `yaml:"Namespace"`

	// The declaration location will be empty if there is no separate definition location
	// (inlined functions or functions not forward-defined). See GetLocation() below.
	DeclLocations []Location `yaml:"Location"`
	DefLocation   Location   `yaml:"DefLocation"`

	Description []CommentInfo   `yaml:"Description"`
	Params      []FieldTypeInfo `yaml:"Params"`
	ReturnType  Type            `yaml:"ReturnType"`
	IsMethod    bool            `yaml:"IsMethod"`
	Access      string          `yaml:"Access"`   // Public, Private, Protected.
	Template    *TemplateInfo   `yaml:"Template"` // Null for non-templates.
}

func (f FunctionInfo) IsPublic() bool {
	return f.Access == "Public"
}
func (f FunctionInfo) IsPrivate() bool {
	// Clang-doc seems to omit access for private members.
	return len(f.Access) == 0 || f.Access == "Private"
}
func (f FunctionInfo) IsProtected() bool {
	return f.Access == "Protected"
}

// Used for getting the canonical location of the function, returns the first declaration location
// or the definition location.
func (f FunctionInfo) GetLocation() Location {
	if len(f.DeclLocations) == 0 {
		return f.DefLocation
	}
	return f.DeclLocations[0]
}

// IdentityKey returns a string which represents the function identity, such that string comparisons
// between functions on their identity keys is sufficient for determining whether two functions are
// the same.
//
// This uses the name, return type, and parameter types. It does not include parameter names nor
// member information (this is used to see if a a function has been covered by a base class so
// we explicitly don't want the enclosing class information).
func (f FunctionInfo) IdentityKey() string {
	result := f.ReturnType.Reference.QualName + " " + f.Name

	// Template specialization args.
	if f.Template != nil && f.Template.Specialization != nil {
		for i, param := range f.Template.Specialization.Params {
			if i > 0 {
				result += ", "
			}
			result += param.Contents
		}
	}

	// Parameter types.
	result += "("
	for i, param := range f.Params {
		if i >= 1 {
			result += ", "
		}
		result += param.TypeRef.QualName
	}
	result += ")"

	return result
}

type EnumValueInfo struct {
	Name  string `yaml:"Name"`
	Value string `yaml:"Value"`
	Expr  string `yaml:"Expr"`
}

type EnumInfo struct {
	USR         string          `yaml:"USR"`
	Name        string          `yaml:"Name"`
	Namespace   []Reference     `yaml:"Namespace"`
	DefLocation Location        `yaml:"DefLocation"`
	Description []CommentInfo   `yaml:"Description"`
	Scoped      bool            `yaml:"Scoped"`   // True for an enum class.
	BaseType    Type            `yaml:"BaseType"` // Defined for explicitly typed enums.
	Members     []EnumValueInfo `yaml:"Members"`
}

type RecordInfo struct {
	Name string `yaml:"Name"`
	USR  string `yaml:"USR"`
	Path string `yaml:"Path"`

	// This will be an array of the path to the struct definition. It will include child
	// structs and not just namespaces.
	//
	// The order is going from the namespace closest to the record and moving outward, so to
	// reconstruct the C++ name you would iterate in reverse.
	//
	//   [ { Type: "Record",    Name: "Outer" },
	//     { Type: "Namespace", Name: "ns" } ]
	//
	// Clang-doc generates "GlobalNamespace"-named namespaces for records in the global
	// namespace. This seems incorrect but was added for the way some other backends work:
	//   https://reviews.llvm.org/D66298
	//
	// If using this to generate a name, "GlobalNamespace" will need to be removed manually.
	Namespace []Reference `yaml:"Namespace"`

	DefLocation Location      `yaml:"DefLocation"`
	Description []CommentInfo `yaml:"Description"`

	TagType string           `yaml:"TagType"`
	Members []MemberTypeInfo `yaml:"Members"`

	Template *TemplateInfo `yaml:"Template"` // Null for non-templates.

	// |Bases| recursively lists all base classes as a way to list the inherited functions.
	// See also |Parents| and |VirtualParents|.
	Bases []*RecordInfo `yaml:"Bases"`

	// The classes that this class derives from directly. Contrast to "Bases" which lists all
	// base classes recursively.
	Parents        []Reference `yaml:"Parents"`
	VirtualParents []Reference `yaml:"VirtualParents"`

	ChildRecordRefs []Reference `yaml:"ChildRecords"`
	ChildRecords    []*RecordInfo

	ChildFunctions []*FunctionInfo `yaml:"ChildFunctions"`
	ChildEnums     []*EnumInfo     `yaml:"ChildEnums"`
	ChildTypedefs  []*TypedefInfo  `yaml:"ChildTypedefs"`

	// When this record is a base class, these items hold the derived information.
	Access    string `yaml:"Access"`
	IsVirtual bool   `yaml:"IsVirtual"`

	// I'm not sure what this means, I suspect this is set when the class also appears in the
	// Parents or VirtualParents list.
	IsParent bool `yaml:"IsParent"`
}

type TypedefInfo struct {
	USR         string        `yaml:"USR"`
	Name        string        `yaml:"Name"`
	Namespace   []Reference   `yaml:"Namespace"`
	Description []CommentInfo `yaml:"Description"`
	DefLocation Location      `yaml:"DefLocation"`
	Underlying  Reference     `yaml:"Underlying"`
	IsUsing     bool          `yaml:"IsUsing"` // False means "typedef".
}

func (r RecordInfo) IsConstructor(f *FunctionInfo) bool {
	return r.Name == f.Name
}
func (r RecordInfo) IsDestructor(f *FunctionInfo) bool {
	return "~"+r.Name == f.Name
}

type NamespaceInfo struct {
	Name string `yaml:"Name"`
	USR  string `yaml:"USR"`

	// "Refs" versions expanded by LoadNamespace().
	ChildNamespaceRefs []Reference `yaml:"ChildNamespaces"`
	ChildNamespaces    []*NamespaceInfo
	ChildRecordRefs    []Reference `yaml:"ChildRecords"`
	ChildRecords       []*RecordInfo

	ChildFunctions []*FunctionInfo `yaml:"ChildFunctions"`
	ChildEnums     []*EnumInfo     `yaml:"ChildEnums"`
	ChildTypedefs  []*TypedefInfo  `yaml:"ChildTypedefs"`
}

// Abstracts how to read files from the input.
type fileReader interface {
	// The input names is relative to the root of where the clang-doc outputs are stored. It
	// may begin with a slash (this still means relative) so that calling code can always
	// prepend a slash when concatenating paths without worrying about whether intermediate
	// directories are present.
	ReadFile(name string) ([]byte, error)
}

// Reader interface implementation for zipped clang-doc outputs.
type zipInput struct {
	reader *zip.Reader
}

func (z zipInput) ReadFile(name string) ([]byte, error) {
	if name != "" && name[0] == '/' {
		name = name[1:]
	}

	file, err := z.reader.Open(name)
	if err != nil {
		return nil, err
	}
	defer file.Close()

	info, err := file.Stat()
	if err != nil {
		return nil, err
	}
	size := info.Size()

	data := make([]byte, 0, size+1)
	for {
		if len(data) >= cap(data) {
			d := append(data[:cap(data)], 0)
			data = d[:len(data)]
		}
		n, err := file.Read(data[len(data):cap(data)])
		data = data[:len(data)+n]
		if err != nil {
			if err == io.EOF {
				err = nil
			}
			return data, err
		}
	}
}

// Reader interface implementation for a regular directory on disk.
type dirInput struct {
	dir string
}

func (d dirInput) ReadFile(name string) ([]byte, error) {
	return os.ReadFile(path.Join(d.dir, name))
}

func LoadRecord(reader fileReader, filename string) *RecordInfo {
	content, err := reader.ReadFile(filename)
	if err != nil {
		log.Fatal(err)
	}

	r := &RecordInfo{}
	err = yaml.Unmarshal(content, &r)
	if err != nil {
		log.Fatalf("error: %v in file %v", err, filename)
	}

	// Child records reference other files in the same directory, named according to the unique
	// USR key.
	for _, c := range r.ChildRecordRefs {
		r.ChildRecords = append(r.ChildRecords, LoadRecord(reader, c.USR+".yaml"))
	}
	return r
}

// LoadNamespace loads everything in the given namespace from the given file.
func LoadNamespace(reader fileReader, filename string) *NamespaceInfo {
	content, err := reader.ReadFile(filename)
	if os.IsNotExist(err) {
		// The file may not exist if the child namespace has no additional attributes
		if Debug {
			log.Printf("WARNING: %v.\n", err)
		}
	}

	ns := &NamespaceInfo{}
	err = yaml.Unmarshal(content, ns)
	if err != nil {
		log.Fatalf("error: %v in file %v", err, filename)
	}

	// The child namespaces and records reference other files in the same directory, named
	// according to the unique USR key.
	for _, c := range ns.ChildNamespaceRefs {
		ns.ChildNamespaces = append(ns.ChildNamespaces,
			LoadNamespace(reader, c.USR+".yaml"))
	}
	for _, c := range ns.ChildRecordRefs {
		ns.ChildRecords = append(ns.ChildRecords,
			LoadRecord(reader, c.USR+".yaml"))
	}

	return ns
}

func loadWithReader(reader fileReader) *NamespaceInfo {
	return LoadNamespace(reader, "index.yaml")
}

// Returns the root namespace. All other namespaces will be inside of this.
func LoadDir(dir string) *NamespaceInfo {
	reader := dirInput{dir}
	return loadWithReader(reader)
}

// Returns the root namespace. All other namespaces will be inside of this.
func LoadZip(zipFile string) *NamespaceInfo {
	reader, err := zip.OpenReader(zipFile)
	if err != nil {
		log.Fatal(err)
	}
	defer reader.Close()

	z := zipInput{&reader.Reader}
	return loadWithReader(z)
}
