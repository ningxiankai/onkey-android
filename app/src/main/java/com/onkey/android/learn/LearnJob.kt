package com.onkey.android.learn

import android.content.Context
import android.media.MediaPlayer
import com.onkey.android.audio.AudioDecoder
import com.onkey.android.native.OnKeyCore
import com.onkey.android.store.LearnedSong
import com.onkey.android.store.SongStore
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.File

/**
 * Orchestrates: decode picked file -> native learn (UVR + CREPE) -> save to library.
 * Progress via [progress] (0..1), status via [phase].
 */
class LearnJob(private val context: Context) {

    sealed class Phase {
        data object Idle : Phase()
        data class Decoding(val fraction: Float) : Phase()
        data class Learning(val fraction: Float) : Phase()
        data class Done(val song: LearnedSong) : Phase()
        data class Failed(val message: String) : Phase()
    }

    private val _progress = MutableStateFlow(0f)
    val progress: StateFlow<Float> = _progress

    private val _phase = MutableStateFlow<Phase>(Phase.Idle)
    val phase: StateFlow<Phase> = _phase

    var job: Job? = null
        private set

    fun start(path: String, title: String, artist: String, scope: CoroutineScope) {
        stop()
        _phase.value = Phase.Decoding(0f)
        _progress.value = 0f
        job = scope.launch(Dispatchers.IO) {
            try {
                val decoded = AudioDecoder.decode(path)
                _phase.value = Phase.Learning(0f)
                val cb = object : Any() {
                    @Suppress("unused")
                    fun onProgress(f: Float) {
                        _progress.value = f
                        _phase.value = Phase.Learning(f)
                    }
                }
                val json = OnKeyCore.nativeLearnSong(
                    OnKeyCore.modelsDir(context).absolutePath,
                    decoded.samples, decoded.rate.toFloat(), cb,
                )
                val o = org.json.JSONObject(json)
                if (o.has("error")) {
                    _phase.value = Phase.Failed(o.getString("error"))
                    return@launch
                }
                val midi = floatArrayOf(o.getJSONArray("midi"))
                val per = floatArrayOf(o.getJSONArray("periodicity"))
                val song = LearnedSong(
                    id = SongStore.newId(),
                    title = title,
                    artist = artist,
                    filePath = path,
                    fps = o.getDouble("fps"),
                    midi = midi,
                    periodicity = per,
                )
                SongStore.save(context, song)
                withContext(Dispatchers.Main) { _phase.value = Phase.Done(song) }
            } catch (e: Exception) {
                _phase.value = Phase.Failed(e.message ?: e.javaClass.simpleName)
            }
        }
    }

    fun stop() {
        job?.cancel()
        job = null
        _phase.value = Phase.Idle
    }

    private fun floatArrayOf(a: org.json.JSONArray?): FloatArray {
        a ?: return FloatArray(0)
        return FloatArray(a.length()) { i -> if (a.isNull(i)) Float.NaN else a.getDouble(i).toFloat() }
    }

    companion object {
        /** Quick self-check used by MainActivity: models present + lib loads. */
        fun health(context: Context): String {
            val ok = OnKeyCore.ensureModels(context)
            OnKeyCore.nativeInit(context.filesDir.absolutePath, context.applicationInfo.nativeLibraryDir)
            return "core=${OnKeyCore.coreVersion()} models=$ok"
        }
    }
}
