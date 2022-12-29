// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package world

import (
	"fmt"
	"sort"
	"strings"

	"go.fuchsia.dev/fuchsia/tools/check-licenses/file"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/project"
)

// CSVData combines some header information along with the data for easily
// producing a CSV file.
//
// This just makes it easier for us to access the information from a template.
type CSVData struct {
	Header  string
	Entries []*CSVEntry
}

type CSVEntry struct {
	Project     string
	Path        string
	LicenseType string
	Url         string
	Package     string
	Left        string
	Right       string
	PatternPath string

	// For compliance worksheet
	BeingSurfaced      string
	SourceCodeIncluded string
}

func NewCSVEntry() {
}

func (csv *CSVEntry) Merge(other *CSVEntry) {
	if !strings.Contains(csv.LicenseType, other.LicenseType+";") {
		csv.LicenseType = fmt.Sprintf("%v; %v", csv.LicenseType, other.LicenseType)
	}
	if !strings.Contains(csv.Package, other.Package+";") {
		csv.Package = fmt.Sprintf("%v; %v", csv.Package, other.Package)
	}
	csv.Left = fmt.Sprintf("%v; %v", csv.Left, other.Left)
}

func (w *World) GetMergedCSVEntries() *CSVData {
	data := w.GetCSVEntries()

	csvMap := make(map[string]*CSVEntry, 0)
	for _, left := range data.Entries {
		if right, ok := csvMap[left.Path]; ok {
			right.Merge(left)
		} else {
			csvMap[left.Path] = left
		}
	}

	newEntries := make([]*CSVEntry, 0)
	for _, csv := range csvMap {
		if csv.Package == csv.Project {
			csv.Project = "Various"
		}
		newEntries = append(newEntries, csv)
	}
	sort.Slice(newEntries, func(i, j int) bool {
		return newEntries[i].Path < newEntries[j].Path
	})
	data.Entries = newEntries
	return data
}

func (w *World) GetCSVEntries() *CSVData {
	csvData := &CSVData{}
	entries := make([]*CSVEntry, 0)

	sort.Sort(project.Order(w.FilteredProjects))
	for _, p := range w.FilteredProjects {
		sort.Sort(file.Order(p.LicenseFile))

		for _, l := range p.LicenseFile {
			sort.Sort(file.OrderFileData(l.Data))

			for _, d := range l.Data {
				e := &CSVEntry{
					Project:            p.Name,
					Path:               d.RelPath,
					Url:                d.URL,
					Package:            d.LibraryName,
					LicenseType:        d.LicenseType,
					BeingSurfaced:      "Yes",
					SourceCodeIncluded: "No",
				}

				if e.Package == "" {
					e.Package = e.Project
				}

				if !p.ShouldBeDisplayed {
					e.BeingSurfaced = "No"
				}

				if p.SourceCodeIncluded {
					e.SourceCodeIncluded = "Yes"
				}

				e.Left = fmt.Sprintf("line %v", d.LineNumber)
				entries = append(entries, e)
			}
		}
	}

	sort.Slice(entries, func(i, j int) bool {
		return entries[i].Path < entries[j].Path
	})
	csvData.Entries = entries
	return csvData
}

// index is the location in a byte slice where a certain text segment
// was found.
//
// newLines is the locations of all the newline characters in the entire
// file.
//
// This method simply returns the line number of the given piece of text,
// without us having to scan the entire file line-by-line.
func getLineNumber(index int, newlines []int) int {
	if index < 0 {
		return index
	}

	largest := 0
	for i, v := range newlines {
		if v == index {
			return i
		}
		if v > index {
			if i > 0 {
				return i - 1
			} else {
				return 0
			}
		}
		largest = i

	}
	return largest
}
