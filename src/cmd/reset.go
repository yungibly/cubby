package cmd

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/spf13/cobra"
)

var (
	resetYes   bool
	resetDryRun bool
)

var resetCmd = &cobra.Command{
	Use:   "reset",
	Short: "Restore all modified files from the storage directory",
	Long: `Restore every file that differs between the storage directory and the
live system, overwriting live files with their stored versions.

Use --dry-run to preview what would be restored without making changes.`,
	RunE: runReset,
}

func init() {
	resetCmd.Flags().BoolVarP(&resetYes, "yes", "y", false, "skip confirmation prompt")
	resetCmd.Flags().BoolVarP(&resetDryRun, "dry-run", "n", false, "simulate without making changes")
	rootCmd.AddCommand(resetCmd)
}

func runReset(cmd *cobra.Command, args []string) error {
	home, _ := os.UserHomeDir()
	store := resolvedStoreDir()

	if _, err := os.Stat(store); os.IsNotExist(err) {
		errorf("storage directory not found: %s", store)
		return nil
	}

	// collect modified files using same logic as status
	var toRestore []statusEntry

	err := filepath.Walk(store, func(storePath string, info os.FileInfo, err error) error {
		if err != nil {
			return nil
		}

		rel, _ := filepath.Rel(store, storePath)
		if rel == "." || isIgnored(rel) {
			if info.IsDir() && rel != "." {
				return filepath.SkipDir
			}
			return nil
		}

		if info.IsDir() {
			return nil
		}

		livePath := filepath.Join(home, rel)
		liveInfo, liveErr := os.Stat(livePath)
		_ = liveInfo

		if os.IsNotExist(liveErr) {
			// missing from live — also restore these
			toRestore = append(toRestore, statusEntry{rel: rel, status: statusMissing})
			return nil
		}

		storeHash, err1 := hashFile(storePath)
		liveHash, err2 := hashFile(livePath)
		if err1 != nil || err2 != nil {
			debugf("could not hash %s", rel)
			return nil
		}

		if string(storeHash) != string(liveHash) {
			toRestore = append(toRestore, statusEntry{rel: rel, status: statusModified})
		}

		return nil
	})

	if err != nil {
		errorf("error walking storage directory: %v", err)
		return nil
	}

	if len(toRestore) == 0 {
		fmt.Printf("%s  %s\n", green("✓"), dim("everything in sync, nothing to reset"))
		return nil
	}

	// preview what will be restored
	fmt.Printf("\n%s\n", bold("files to restore:"))
	for _, e := range toRestore {
		label := "modified"
		if e.status == statusMissing {
			label = "missing"
		}
		fmt.Printf("  %s  %s  %s\n", cyan("↓"), e.rel, dim("("+label+")"))
	}
	fmt.Println()

	if resetDryRun {
		fmt.Printf("%s\n", dim(fmt.Sprintf("%d files would be restored", len(toRestore))))
		return nil
	}

	if !resetYes {
		if !confirm(fmt.Sprintf("restore %d files from storage?", len(toRestore))) {
			fmt.Println(dim("aborted."))
			return nil
		}
	}

	// restore each file
	errors := 0
	for _, e := range toRestore {
		src := filepath.Join(store, e.rel)
		dst := filepath.Join(home, e.rel)

		if err := os.MkdirAll(filepath.Dir(dst), 0755); err != nil {
			errorf("could not create directory for %s: %v", e.rel, err)
			errors++
			continue
		}
		if err := copyFile(src, dst); err != nil {
			errorf("restore failed for %s: %v", e.rel, err)
			errors++
			continue
		}
		printRestore(e.rel, store)
		logEntry("restore", e.rel)
	}

	fmt.Printf("\n%s\n", dim(fmt.Sprintf(
		"%d restored  ·  %d errors",
		len(toRestore)-errors, errors,
	)))

	return nil
}
