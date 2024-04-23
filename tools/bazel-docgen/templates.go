// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package bazel_docgen

import (
	"text/template"

	pb "go.fuchsia.dev/fuchsia/tools/bazel-docgen/third_party/stardoc"
)

// TODO: this is just stubbed out for now
const ruleTemplate = `
[TOC]

# {{ .RuleName }}

{{ .DocString }}

## **ATTRIBUTES**

| Name  | Description | Type | Mandatory | Default |
| :------------- | :------------- | :------------- | :------------- | :------------- |
{{ range .Attribute }}| {{ .Name }} | {{description .DocString }} | {{attributeTypeString .Type }} | {{isMandatory .Mandatory }} | {{defaultValue .DefaultValue }} |
{{ end }}
`

// TODO: these are all just stubs, need actual implementation
const providerTemplate = ""
const starlarkFunctionTemplate = ""
const repositoryRuleTemplate = ""

var (
	attributeTypeURLMap = map[pb.AttributeType]string{
		pb.AttributeType_INT:               "int",
		pb.AttributeType_LABEL:             "label",
		pb.AttributeType_STRING:            "string",
		pb.AttributeType_STRING_LIST:       "string_list",
		pb.AttributeType_INT_LIST:          "int_list",
		pb.AttributeType_LABEL_LIST:        "label_list",
		pb.AttributeType_BOOLEAN:           "bool",
		pb.AttributeType_LABEL_STRING_DICT: "label_keyed_string_dict",
		pb.AttributeType_STRING_DICT:       "string_dict",
		pb.AttributeType_STRING_LIST_DICT:  "string_list_dict",
		pb.AttributeType_OUTPUT:            "output",
		pb.AttributeType_OUTPUT_LIST:       "output_list",
	}
	attributeTypeReadableNameMap = map[pb.AttributeType]string{
		pb.AttributeType_INT:               "Integer",
		pb.AttributeType_LABEL:             "Label",
		pb.AttributeType_STRING:            "String",
		pb.AttributeType_STRING_LIST:       "String List",
		pb.AttributeType_INT_LIST:          "Integer List",
		pb.AttributeType_LABEL_LIST:        "Label List",
		pb.AttributeType_BOOLEAN:           "Boolean",
		pb.AttributeType_LABEL_STRING_DICT: "Label String Dict",
		pb.AttributeType_STRING_DICT:       "String Dict",
		pb.AttributeType_STRING_LIST_DICT:  "String List Dict",
		pb.AttributeType_OUTPUT:            "Output",
		pb.AttributeType_OUTPUT_LIST:       "Output List",
	}
)

func defaultValue(value string) string {
	if value == "" {
		return "-"
	}
	// Return the value with backticks if there is one
	return "`" + value + "`"
}

func description(value string) string {
	if value == "" {
		return "-"
	}
	return value
}

func attributeTypeString(t pb.AttributeType) string {
	if t == pb.AttributeType_NAME {
		return "<a href=\"https://bazel.build/concepts/labels#target-names\">Name</a>"
	} else if t == pb.AttributeType_UNKNOWN {
		return "UKNOWN"
	} else {
		return "<a href=\"https://bazel.build/rules/lib/toplevel/attr#" + attributeTypeURLMap[t] + "\">" + attributeTypeReadableNameMap[t] + "</a>"
	}
}

func isMandatory(m bool) string {
	if m {
		return "required"
	} else {
		return "optional"
	}
}
func NewRuleTemplate() (*template.Template, error) {
	return makeTemplate("rule", ruleTemplate)
}

func NewProviderTemplate() (*template.Template, error) {
	return makeTemplate("provider", providerTemplate)
}

func NewStarlarkFunctionTemplate() (*template.Template, error) {
	return makeTemplate("starlark_function", starlarkFunctionTemplate)
}

func NewRepositoryRuleTemplate() (*template.Template, error) {
	return makeTemplate("repository_rule", repositoryRuleTemplate)
}

func makeTemplate(name string, templateString string) (*template.Template, error) {
	t := template.New(name)
	t.Funcs(
		template.FuncMap{
			"defaultValue":        defaultValue,
			"description":         description,
			"attributeTypeString": attributeTypeString,
			"isMandatory":         isMandatory,
		},
	)
	return t.Parse(templateString)
}
