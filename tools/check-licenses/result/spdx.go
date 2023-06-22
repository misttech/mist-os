// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package result

import (
	"encoding/json"
	"fmt"
	"path/filepath"
	"strings"

	"go.fuchsia.dev/fuchsia/tools/check-licenses/project"

	spdx_common "github.com/spdx/tools-golang/spdx/common"
	spdx "github.com/spdx/tools-golang/spdx/v2_2"
)

const (
	spdxProjectName = "Fuchsia"
	spdxFilename    = "results.spdx.json"
)

func generateSPDXDoc(name string, projects []*project.Project, root *project.Project) (string, error) {
	seenOtherLicenses := make(map[string]*spdx.OtherLicense, 0)

	if root == nil {
		return "", fmt.Errorf("root project must not be nil")
	}

	rootPackage, err := root.GenerateSPDXPackage()
	if err != nil {
		return "", fmt.Errorf("failed to generate SPDX package for root package %s: %w",
			root.Name, err)
	}

	var b strings.Builder
	b.WriteString("\n")

	doc := &spdx.Document{
		SPDXVersion:       "SPDX-2.2",
		DataLicense:       "CC0-1.0",
		SPDXIdentifier:    "SPDXRef-DOCUMENT",
		DocumentName:      Config.SPDXDocName,
		DocumentNamespace: "fuchsia--{da39a3ee5e6b4b0d3255bfef95601890afd80709 []}",
		Files:             make([]*spdx.File, 0),
		Annotations:       make([]*spdx.Annotation, 0),
		Snippets:          make([]spdx.Snippet, 0),
		Reviews:           nil,
		Packages:          make([]*spdx.Package, 0),
		Relationships:     make([]*spdx.Relationship, 0),
		OtherLicenses:     make([]*spdx.OtherLicense, 0),
		CreationInfo: &spdx.CreationInfo{
			Creators: []spdx_common.Creator{
				{
					Creator:     "fuchsia.googlesource.com/fuchsia/+/refs/head/main/tools/check-licenses",
					CreatorType: "Tool",
				},
			},
		},
	}

	// Every SPDX doc must have one "DESCRIBES" relationship.
	r := &spdx.Relationship{
		RefA:         spdx_common.DocElementID{ElementRefID: doc.SPDXIdentifier},
		RefB:         spdx_common.DocElementID{ElementRefID: rootPackage.PackageSPDXIdentifier},
		Relationship: "DESCRIBES",
	}
	doc.Relationships = append(doc.Relationships, r)

	for _, p := range projects {
		pPackage, err := p.GenerateSPDXPackage()
		if err != nil {
			return "", fmt.Errorf("failed to generate SPDX package for project %s: %w", p.Name, err)
		}
		doc.Packages = append(doc.Packages, pPackage)

		if p != root {
			r := &spdx.Relationship{
				RefA:         spdx_common.DocElementID{ElementRefID: rootPackage.PackageSPDXIdentifier},
				RefB:         spdx_common.DocElementID{ElementRefID: pPackage.PackageSPDXIdentifier},
				Relationship: "CONTAINS",
			}
			doc.Relationships = append(doc.Relationships, r)
		}

		for _, l := range p.LicenseFiles {
			// Prebuilts often come with a NOTICE file with several license texts.
			// For now, let's keep those license texts together in one single
			// OtherLicense SPDX object.
			text, err := l.Text()
			if err != nil {
				return "", fmt.Errorf("Failed to get text for file %s: %v", l.RelPath(), err)
			}
			data, err := l.Data()
			if err != nil {
				return "", fmt.Errorf("Failed to get data for file %s: %v", l.RelPath(), err)
			}
			if strings.Contains(p.Root, "prebuilt") {
				ol := &spdx.OtherLicense{
					LicenseName:       l.SPDXName(),
					LicenseIdentifier: l.SPDXID(),
					ExtractedText:     string(text),
					LicenseCrossReferences: []string{
						l.URL(),
					},
				}
				if _, ok := seenOtherLicenses[ol.LicenseIdentifier]; !ok {
					seenOtherLicenses[ol.LicenseIdentifier] = ol
					doc.OtherLicenses = append(doc.OtherLicenses, ol)
				}
				continue
			}

			for _, d := range data {
				ol := &spdx.OtherLicense{
					LicenseName:       d.SPDXName(),
					LicenseIdentifier: d.SPDXID(),
					ExtractedText:     string(d.Data()),
					LicenseCrossReferences: []string{
						d.URL(),
					},
				}
				if _, ok := seenOtherLicenses[ol.LicenseIdentifier]; !ok {
					seenOtherLicenses[ol.LicenseIdentifier] = ol
					doc.OtherLicenses = append(doc.OtherLicenses, ol)
				}
			}
		}
	}

	buf, err := json.MarshalIndent(doc, "", "  ")
	if err != nil {
		return "", fmt.Errorf("failed to marshal SPDX document: %w", err)
	}

	b.WriteString(fmt.Sprintf(" ⦿ Generated SPDX file -> %v", filepath.Join(Config.OutDir, spdxFilename)))
	b.WriteString("\n")

	return b.String(), writeFile(spdxFilename, buf)
}
