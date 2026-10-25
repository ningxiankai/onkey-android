package com.onkey.android.native

import android.content.Context
import java.io.File

/**
 * JNI bridge to the onkey_core Rust library (OnKey's pipeline, MIT, github.com/dsmurrell/onkey).
 */
object OnKeyCore {
    init {
        System.loadLibrary("onkey_core")
    }

    external fun coreVersion(): String

    /** Call once at app start: dataDir = writable dir; nativeLibDir = applicationInfo.nativeLibraryDir (has libonnxruntime.so). */
    external fun nativeInit(dataDir: String, nativeLibDir: String)

    /**
     * Learn a song. [stereo] is interleaved f32 at [rate]; blocks — run on a worker
     * thread. [progress].onProgress(f: Float) is called with 0..1 as chunks finish.
     * Returns JSON: {"fps":20,"midi":[...],"periodicity":[...]} or {"error":"..."}.
     */
    external fun nativeLearnSong(modelsDir: String, stereo: FloatArray, rate: Float, progress: Any): String

    /** Live mic pitch: [x] is mono 16 kHz f32; returns midi (NaN when unvoiced). */
    external fun nativePitchFrame(x: FloatArray): Float

    fun modelsDir(context: Context): File =
        File(context.filesDir, "models").apply { mkdirs() }

    /**
     * Copy bundled models from assets to filesDir (ONNX Runtime needs real paths).
     * Idempotent: skipped when the file already exists with the right size.
     */
    fun ensureModels(context: Context): Boolean {
        val dir = modelsDir(context)
        var ok = true
        for (name in listOf("UVR_MDXNET_KARA_2.onnx", "crepe-full-b32.onnx")) {
            val out = File(dir, name)
            val expected = try {
                context.assets.openFd("models/$name").length
            } catch (e: Exception) {
                -1L
            }
            if (out.isFile && expected < 0 || (out.isFile && out.length() == expected)) continue
            try {
                context.assets.open("models/$name").use { input ->
                    val tmp = File(dir, "$name.part")
                    tmp.outputStream().use { output -> input.copyTo(output) }
                    tmp.renameTo(out) || (out.delete() && tmp.renameTo(out))
                }
            } catch (e: Exception) {
                ok = false
            }
        }
        return ok
    }
}
