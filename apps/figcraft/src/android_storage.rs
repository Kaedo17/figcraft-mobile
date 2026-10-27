//! Android's app-private storage, below the `AndroidApp` data directory.
//!
//! Android has no system file picker the app can drive (rfd has no backend there), so the built-in
//! file browser serves every file command over the app-private storage: no permissions and no
//! Storage Access Framework round-trip, though invisible to the Files app. Shared internal storage
//! is the browser's second root, entered only while Android's "All files access" grant holds
//! (checked and asked for by `apps/figcraft-android`'s `android_access.rs` over JNI). Pure path
//! logic, unit-tested on the host; the data directory comes from [`crate::run_inner`], which
//! records it before anything resolves the config folder.
//!
//! The app's own folder below the data directory is the config root ([`crate::services::config_dir`]
//! on Android): preferences, the recovery copies and the installed plugins. The browser's Documents
//! folder sits beside them: where the browser starts and where saves land.

// Only Android's launch resolves and creates these; the host build compiles the module for the
// tests below.
#![cfg_attr(not(target_os = "android"), allow(dead_code))]

use std::path::{Path, PathBuf};

/// Shared internal storage's root on Android: the in-app browser's second root
/// (`Services::shared_storage_root`), entered only while the "All files access" grant holds —
/// `apps/figcraft-android`'s `android_access.rs` checks and asks for it over JNI.
pub const SHARED_ROOT: &str = "/storage/emulated/0";

/// The storage folders below the app-private data directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Layout {
    /// The app's folder: the config root — preferences, the recovery copies and the installed
    /// plugins — and what the in-app browser may reach (`Services::app_files_dir`).
    pub(crate) base: PathBuf,
    /// Where the in-app browser starts and where saves and exports default to.
    pub(crate) documents: PathBuf,
}

/// Split the app-private data directory into the app's folders.
pub(crate) fn layout(internal: &Path) -> Layout {
    let base = internal.join("FigCraft");
    Layout { base: base.clone(), documents: base.join("Documents") }
}

/// Create the folders (`Err` names the first failure; the app then keeps that feature off rather
/// than crashing).
pub(crate) fn ensure(layout: &Layout) -> Result<(), String> {
    for dir in [&layout.base, &layout.documents] {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.to_string_lossy()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_base(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("figcraft-android-storage-{tag}-{}", std::process::id()))
    }

    #[test]
    fn layout_puts_documents_beside_the_config_root() {
        let internal = Path::new("/data/user/0/ai.storyteller.figcraft/files");
        let l = layout(internal);
        assert_eq!(l.base, internal.join("FigCraft"));
        assert_eq!(l.documents, internal.join("FigCraft").join("Documents"));
        // The config root is the app's folder and Documents sits directly below it.
        assert_eq!(l.documents.parent(), Some(l.base.as_path()));
    }

    #[test]
    fn ensure_creates_every_folder() {
        let internal = temp_base("ok");
        let _ = std::fs::remove_dir_all(&internal);
        let l = layout(&internal);
        ensure(&l).unwrap();
        assert!(l.base.is_dir() && l.documents.is_dir());
        // Re-running over existing folders is fine.
        ensure(&l).unwrap();
        let _ = std::fs::remove_dir_all(&internal);
    }

    #[test]
    fn ensure_reports_a_folder_it_could_not_create() {
        let internal = temp_base("blocked");
        let _ = std::fs::remove_dir_all(&internal);
        // a file where a folder has to go: create_dir_all must fail, not panic
        std::fs::create_dir_all(&internal).unwrap();
        std::fs::write(internal.join("FigCraft"), b"x").unwrap();
        let err = ensure(&layout(&internal)).unwrap_err();
        assert!(err.contains("FigCraft"), "{err}");
        let _ = std::fs::remove_dir_all(&internal);
    }
}
