package cmd

import (
	"fmt"
	"os"
	"path/filepath"

	"github.com/spf13/cobra"
)

var (
	saveYes    bool
	saveDryRun bool
)

var saveCmd = &cobra.Command{
	Use:   "save",
	Short: "Sync all modified live files to the storage directory",
	Long: `Sync every file that differs between the live system and the storage
directory, updating stored versions with live ones.
The natural opposite of cubby reset.

Use --dry-run to preview what would be synced without making changes.`,
	RunE: runSave,
}

func init() {
	saveCmd.Flags().BoolVarP(&saveYes, "yes", "y", false, "skip confirmation prompt")
	saveCmd.Flags().BoolVarP(&saveDryRun, "dry-run", "n", false, "simulate without making changes")
	rootCmd.AddCommand(saveCmd)
}

func runSave(cmd *cobra.Command, args []string) error {
	home, _ := os.UserHomeDir()
	store := resolvedStoreDir()

	if _, err := os.Stat(store); os.IsNotExist(err) {
		errorf("storage directory not found: %s", store)
		return nil
	}

	var toSave []statusEntry

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
		if _, err := os.Stat(livePath); os.IsNotExist(err) {
			// missing from live — skip, that's reset's territory
			return nil
		}

		storeHash, err1 := hashFile(storePath)
		liveHash, err2 := hashFile(livePath)
		if err1 != nil || err2 != nil {
			debugf("could not hash %s", rel)
			return nil
		}

		if string(storeHash) != string(liveHash) {
			toSave = append(toSave, statusEntry{rel: rel, status: statusModified})
		}

		return nil
	})

	if err != nil {
		errorf("error walking storage directory: %v", err)
		return nil
	}

	if len(toSave) == 0 {
		fmt.Printf("%s  %s\n", green("✓"), dim("everything in sync, nothing to save"))
		return nil
	}

	// preview
	fmt.Printf("\n%s\n", bold("files to save:"))
	for _, e := range toSave {
		fmt.Printf("  %s  %s\n", green("✓"), e.rel)
	}
	fmt.Println()

	if saveDryRun {
		fmt.Printf("%s\n", dim(fmt.Sprintf("%d files would be saved", len(toSave))))
		return nil
	}

	if !saveYes {
		if !confirm(fmt.Sprintf("save %d files to storage?", len(toSave))) {
			fmt.Println(dim("aborted."))
			return nil
		}
	}

	errors := 0
	for _, e := range toSave {
		src := filepath.Join(home, e.rel)
		dst := filepath.Join(store, e.rel)

		if err := os.MkdirAll(filepath.Dir(dst), 0755); err != nil {
			errorf("could not create directory for %s: %v", e.rel, err)
			errors++
			continue
		}
		if err := copyFile(src, dst); err != nil {
			errorf("save failed for %s: %v", e.rel, err)
			errors++
			continue
		}
		printSync(e.rel, store)
		logEntry("sync", e.rel)
	}

	fmt.Printf("\n%s\n", dim(fmt.Sprintf(
		"%d saved  ·  %d errors",
		len(toSave)-errors, errors,
	)))

	return nil
}
