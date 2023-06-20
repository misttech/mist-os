// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

package project

import (
	"fmt"
	"runtime"
	"sort"
	"sync"

	"go.fuchsia.dev/fuchsia/tools/check-licenses/file"
	"go.fuchsia.dev/fuchsia/tools/check-licenses/license"
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
		if useLicenseClassifier {
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
			continue
		}

		for _, l := range p.LicenseFiles {
			if results, err := license.Search(p.Root, l); err != nil {
				return fmt.Errorf("Issue analyzing Project defined in [%v]: %v\n", p.ReadmeFile.ReadmePath, err)
			} else {
				p.LicenseFileSearchResults = append(p.LicenseFileSearchResults, results...)
				for _, r := range results {
					key := string(r.LicenseData.Data())
					if _, ok := p.LicenseFileSearchResultsDeduped[key]; !ok {
						p.LicenseFileSearchResultsDeduped[key] = r
					}
				}
			}
			// Set the license URLs in the license file objects.
			l.UpdateURLs(p.Name, p.URL)
		}
		sort.Sort(license.SearchResultOrder(p.LicenseFileSearchResults))

		// Currently, searching copyright header info for all source files
		// in all projects is too much work. Runtimes on my local machine exceed 30mins.
		//
		// TODO(fxbug.dev/125491): Enable checks on all source files.
		if p.Root != "." {
			continue
		}

		// Analyze the copyright headers in the files in each project.
		sort.Sort(file.Order(p.SearchableRegularFiles))
		for _, f := range p.SearchableRegularFiles {
			text, _ := f.Text()
			if len(text) == 0 {
				continue
			}
			if results, err := license.SearchHeaders(p.Root, f); err != nil {
				return fmt.Errorf("Issue analyzing Project defined in [%v]: %v\n", p.ReadmeFile.ReadmePath, err)
			} else {
				p.RegularFileSearchResults = append(p.RegularFileSearchResults, results...)
			}
		}
		sort.Sort(license.SearchResultOrder(p.RegularFileSearchResults))
	}
	wg.Wait()

	// Perform any cleanup steps in the license package.
	license.Finalize()
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
