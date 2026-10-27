//! Android entry point for FigCraft.
//!
//! Android's `NativeActivity` loads this library and calls [`android_main`] on a dedicated
//! thread. It opens the shared app ([`figcraft::run_android`], the same code the desktop binary
//! runs) and returns when the window closes, ending the activity's main-loop thread.
//!
//! This crate exists because that export must be unmangled, and on edition 2024 the only spelling
//! (`#[unsafe(no_mangle)]`) trips the workspace-forbidden `unsafe_code` lint — see `Cargo.toml`
//! for why the crate carries its own lints instead.
//!
//! Android limitations (v1): no command line, so the launch is the default one — no control
//! server, no automation roots, no files to open and no invite links. No system file dialogs and
//! no system clipboard beyond egui's text (rfd and arboard have no backend for the target), so
//! Open/Save/export answer cancelled until FigCraft's own in-app file browser serves them (a later
//! step), and there is no per-user settings folder (`HOME` and `XDG_CONFIG_HOME` are unset):
//! preferences, recovery copies and installed plugins live in the app-private storage directory
//! the activity resolves.
//!
//! On every other target this crate is empty: only Android's loader ever looks for `android_main`.

#[cfg(target_os = "android")]
mod android_access;

/// NativeActivity's main-loop thread entry point (required; see the `android-activity` docs).
/// winit pumps the activity's lifecycle and input events inside [`figcraft::run_android`], which
/// needs the activity's handle to create its event loop; when the activity is destroyed that
/// returns and this returns with it.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(app: android_activity::AndroidApp) {
    // The "All files access" grant, checked and requested over JNI — this crate's unsafe stays
    // in `android_access`; `run_android` only ever sees the plain closures built here.
    let shared = android_access::capture(&app).map(|a| figcraft::SharedStorageAccess {
        granted: std::sync::Arc::new(move || a.shared_storage_granted()),
        ask: std::sync::Arc::new(move || a.ask_shared_storage_grant()),
    });
    if let Err(e) = figcraft::run_android(app, shared) {
        log::error!("figcraft exited with an error: {e}");
    }
}
