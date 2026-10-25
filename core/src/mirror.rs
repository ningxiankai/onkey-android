//! ANDROID STUB for OnKey's development mirror (a dev-only TCP server for watching the
//! app's window from a browser). No-op on Android; keeps devtest.rs compiling unchanged.

/// An event the engine sent: no tabs to feed on Android.
pub fn send(_kind: &str, _json: &str) {}

/// The mirror server is not started on Android.
pub fn start(_store: crate::store::Store) {}
