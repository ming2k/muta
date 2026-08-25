#[cfg(test)]
mod tests {
    use super::super::*;
    use muta_contracts::WorktreeMode;

    #[test]
    fn shadow_worktree_creation_and_diff_tracking() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base_dir = tmp.path();

        // Create initial file in base
        let cargo_toml = base_dir.join("Cargo.toml");
        std::fs::write(&cargo_toml, "[package]\nname = \"test_pkg\"\n").expect("write file");

        let mut ws = IsolatedWorkspace::create(base_dir, WorktreeMode::Branch, "test_ws_1")
            .expect("create workspace");
        assert!(ws.path().exists());

        // Shadow workspace should have copied Cargo.toml
        let shadow_cargo = ws.path().join("Cargo.toml");
        assert!(shadow_cargo.exists());

        // Add a new file and modify existing file in shadow workspace
        let shadow_src = ws.path().join("src");
        std::fs::create_dir_all(&shadow_src).expect("mkdir");
        let shadow_main = shadow_src.join("main.rs");
        std::fs::write(&shadow_main, "fn main() { println!(\"hello\"); }").expect("write main");

        std::fs::write(
            &shadow_cargo,
            "[package]\nname = \"test_pkg\"\nversion = \"0.2.0\"\n",
        )
        .expect("edit cargo");

        let modified = ws.list_modified_files();
        assert_eq!(modified.len(), 2);

        // Apply changes back to base
        let applied = ws.apply_to_target(base_dir).expect("apply to base");
        assert_eq!(applied.len(), 2);

        let base_main = base_dir.join("src/main.rs");
        assert!(base_main.exists());
        let content = std::fs::read_to_string(base_main).expect("read applied main");
        assert_eq!(content, "fn main() { println!(\"hello\"); }");

        // Clean up
        ws.cleanup().expect("cleanup");
        assert!(!ws.path().exists());
    }
}
