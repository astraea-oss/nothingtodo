//! Android-only process setup.
use jni::{JavaVM, objects::JObject};

/// HTTPS certificates are checked against Android's trust store by rustls-platform-verifier,
/// which panics on its first use unless it has been given the JVM and an app context. Tauri has
/// already published both through `ndk-context` by the time `setup` runs.
pub fn init_tls() -> Result<(), String> {
    let android = ndk_context::android_context();
    // SAFETY: ndk-context holds a valid JavaVM pointer and a global reference to the activity for
    // the life of the process. Neither wrapper releases anything when dropped.
    let vm = unsafe { JavaVM::from_raw(android.vm().cast()) };
    vm.attach_current_thread(|env| {
        let context = unsafe { JObject::from_raw(env, android.context().cast()) };
        rustls_platform_verifier::android::init_with_env(env, context)
    })
    .map_err(|e| format!("Could not set up HTTPS certificate checks: {e}"))
}
