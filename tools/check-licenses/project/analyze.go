// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package project

import (
	"runtime"
	"sort"
	"sync"

	"go.fuchsia.dev/fuchsia/tools/check-licenses/file"
)

// AnalyzeLicenses loops over every project that was created during this run,
// and performs a license search on the licenses and regular files included
// in each project.
func AnalyzeLicenses() error {
	// Convert the projects map into a list and sort it, to make this function consistent.
	filteredProjectsList := make([]*Project, 0, len(FilteredProjects))
	for _, p := range FilteredProjects {
		filteredProjectsList = append(filteredProjectsList, p)
	}
	sort.Sort(Order(filteredProjectsList))

	var wg sync.WaitGroup
	for _, p := range filteredProjectsList {
		plusVal(NumFilteredProjects, p.Root)

		// Analyze the license files in each project.
		sort.Sort(file.Order(p.LicenseFiles))

		wg.Add(1)
		go func(project *Project) {
			defer wg.Done()
			var pwg sync.WaitGroup

			// Analyze license files.
			for _, l := range project.LicenseFiles {
				l.Search()
			}

			sort.Sort(file.Order(project.SearchableRegularFiles))

			// Analyze the copyright headers in the non-license files in each project.
			filesPerCPU := max(len(project.SearchableRegularFiles)/runtime.NumCPU(), 1)
			for i := 0; i < len(project.SearchableRegularFiles); i = i + filesPerCPU {
				pwg.Add(1)
				go func(start, end int) {
					defer pwg.Done()
					for _, f := range project.SearchableRegularFiles[start:end] {
						f.Search()
					}
				}(i, min(i+filesPerCPU, len(project.SearchableRegularFiles)))
			}
			pwg.Wait()
		}(p)
	}
	wg.Wait()
	return nil
}

func min(a, b int) int {
	if a < b {
		return a
	}
	return b
}

func max(a, b int) int {
	if a < b {
		return b
	}
	return a
}
