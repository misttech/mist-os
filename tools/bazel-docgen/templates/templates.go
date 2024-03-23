// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package templates

import (
	"text/template"
)

// TODO: this is just stubbed out for now
const ruleTemplate = `
## {{ .RuleName }}

{{ .DocString }}

**ATTRIBUTES**

{{ range .Attribute }} - {{ .Name }}
{{ end }}
`

func NewRuleTemplate() (*template.Template, error) {
	return makeTemplate("rule", ruleTemplate)
}

func makeTemplate(name string, templateString string) (*template.Template, error) {
	t := template.New(name)
	return t.Parse(templateString)
}
