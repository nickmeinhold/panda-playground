// SPDX-License-Identifier: MIT
//
// JNI handshake that hands the Activity context to `ndk_context` so that any
// downstream Rust crate (iroh-net's interface enumeration, mDNS multicast-lock
// acquisition, etc.) can reach Android system services. Without this, the
// first `ndk_context::android_context()` call panics with
// "android context was not initialized" — observed during `Gossip::spawn`
// from `node::Node::start`.
//
// Wire path: `android/app/src/main/kotlin/.../MainActivity.kt` calls
// `NdkContextInit.init(applicationContext)` early in `configureFlutterEngine`,
// which routes through the JNI symbol below.

use jni::objects::{JClass, JObject};
use jni::sys::jint;
use jni::{JNIEnv, JavaVM};

/// JNI export wired to `org.p2panda.panda_playground.NdkContextInit.init`.
///
/// The mangled name is constructed per the JNI naming rules:
///   - package separators `.` become `_`
///   - underscores in identifiers are escaped as `_1`
///   - class and method names are appended with `_`
/// So `org.p2panda.panda_playground.NdkContextInit.init` →
///   `Java_org_p2panda_panda_1playground_NdkContextInit_init`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_p2panda_panda_1playground_NdkContextInit_init(
    mut env: JNIEnv,
    _class: JClass,
    context: JObject,
) {
    // Initialize android_logger first so the result of this handshake
    // actually reaches logcat. `init_logging` is idempotent (uses
    // `android_logger::init_once`), so calling it again from `start_node`
    // later is harmless.
    crate::api::chat::init_logging();

    if let Err(err) = init_inner(&mut env, context) {
        log::error!("[android_init] failed to register ndk-context: {err}");
    } else {
        log::info!("[android_init] ndk-context registered");
    }
}

fn init_inner(env: &mut JNIEnv, context: JObject) -> Result<(), String> {
    let vm: JavaVM = env
        .get_java_vm()
        .map_err(|e| format!("get_java_vm: {e}"))?;
    let global_ctx = env
        .new_global_ref(context)
        .map_err(|e| format!("new_global_ref(context): {e}"))?;

    // SAFETY: We hand `ndk-context` raw pointers it expects to outlive the
    // process. The `GlobalRef` is intentionally leaked so the Context's
    // JNI handle stays valid for the lifetime of the app — releasing it
    // would let the JVM collect the Context that downstream crates hold.
    unsafe {
        let vm_ptr = vm.get_java_vm_pointer() as *mut std::ffi::c_void;
        let ctx_ptr = global_ctx.as_obj().as_raw() as *mut std::ffi::c_void;
        std::mem::forget(global_ctx);
        ndk_context::initialize_android_context(vm_ptr, ctx_ptr);
    }
    Ok(())
}

/// Idempotency guard — called from `init_logging` so multiple `start_node`
/// calls don't try to re-register. Currently a no-op (the Kotlin side does
/// the registration before any Dart code runs), but documents the contract.
#[allow(dead_code)]
pub(crate) fn assert_registered() {
    // ndk-context exposes no "is initialized" check; we assume Kotlin ran
    // `NdkContextInit.init(...)` and rely on the panic-hook to surface any
    // ordering violation rather than silently failing.
}

/// `JNI_OnLoad` is called by the dynamic linker when our `.so` is loaded.
/// We don't strictly need it for the context handshake (Kotlin will call
/// `NdkContextInit.init` explicitly), but exposing it advertises support
/// for JNI version 1.6 and silences a warning some Android loaders emit.
#[unsafe(no_mangle)]
pub extern "system" fn JNI_OnLoad(_vm: JavaVM, _reserved: *mut std::ffi::c_void) -> jint {
    jni::sys::JNI_VERSION_1_6
}
