// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package docgen

import (
	"fmt"
	"go.fuchsia.dev/fuchsia/tools/cppdocgen/clangdoc"
	"io"
)

func typedefHtmlId(t *clangdoc.TypedefInfo) string {
	return getScopeQualifier(t.Namespace, true) + t.Name
}

func typedefLink(t *clangdoc.TypedefInfo) string {
	return HeaderReferenceFile(t.DefLocation.Filename) + "#" + typedefHtmlId(t)
}

func typedefFullName(t *clangdoc.TypedefInfo) string {
	return getScopeQualifier(t.Namespace, true) + t.Name
}

func writeTypedefName(t *clangdoc.TypedefInfo, f io.Writer) {
	fmt.Fprintf(f, "<span class=\"typ\">%s</span>", t.Name)
}

func writeTypedefUnderlying(t *clangdoc.TypedefInfo, f io.Writer) {
	tn, _ := getEscapedTypeName(t.Underlying.QualName)
	fmt.Fprintf(f, "<span class=\"typ\">%s</span>", tn)
}

// Writes the typedef/using line assuming the <pre> and namespace information has already
// been output.
func writeTypedefDeclarationLine(t *clangdoc.TypedefInfo, f io.Writer) {
	if t.IsUsing {
		fmt.Fprintf(f, "<span class=\"kwd\">using</span> ")
		writeTypedefName(t, f)
		fmt.Fprintf(f, " = ")
		writeTypedefUnderlying(t, f)
		fmt.Fprintf(f, ";\n")
	} else {
		fmt.Fprintf(f, "<span class=\"kwd\">typedef</span> ")
		writeTypedefUnderlying(t, f)
		fmt.Fprintf(f, " ")
		writeTypedefName(t, f)
		fmt.Fprintf(f, ";\n")
	}
}

func writeTypedefDeclaration(t *clangdoc.TypedefInfo, f io.Writer) {
	writePreHeader(f)

	nsBegin, nsEnd := getNamespaceDecl(t.Namespace)
	fmt.Fprintf(f, "%s", nsBegin)

	writeTypedefDeclarationLine(t, f)

	fmt.Fprintf(f, "%s", nsEnd)
	writePreFooter(f)
}

func writeTypedefSection(settings WriteSettings, index *Index, t *clangdoc.TypedefInfo, f io.Writer) {
	headingLine, _ := extractCommentHeading1(t.Description)
	if headingLine == "" {
		fmt.Fprintf(f, "## %s typedef {:#%s}\n\n", t.Name, typedefHtmlId(t))
	} else {
		// Explicit title. Add a "#" to make it "level 2".
		fmt.Fprintf(f, "#%s {:#%s}\n\n", headingLine, typedefHtmlId(t))
	}

	fmt.Fprintf(f, "[Declaration source code](%s)\n\n", settings.locationSourceLink(t.DefLocation))

	if !commentContains(t.Description, NoDeclTag) {
		writeTypedefDeclaration(t, f)
	}
	writeComment(index, t.Description, markdownHeading2, f)

	fmt.Fprintf(f, "\n")
}
