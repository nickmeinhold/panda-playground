package org.p2panda.panda_playground

import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine

/// Bridge to the Rust `Java_org_p2panda_panda_1playground_NdkContextInit_init`
/// symbol exported from `rust/src/android_init.rs`. Loading the library here
/// rather than relying on flutter_rust_bridge's lazy load guarantees the
/// JNI handshake runs before any Dart code calls into Rust (`startNode`,
/// which transitively touches iroh-net's interface enumeration and would
/// otherwise panic with "android context was not initialized").
object NdkContextInit {
    init {
        System.loadLibrary("rust_lib_panda_playground")
    }

    @JvmStatic external fun init(context: Any)
}

class MainActivity : FlutterActivity() {
    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        NdkContextInit.init(applicationContext)
    }
}
