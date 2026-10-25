package com.onkey.android.audio

import android.annotation.SuppressLint
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import com.onkey.android.native.OnKeyCore

/**
 * Mic recorder feeding 16 kHz mono chunks to the Rust live-pitch tracker.
 * [onPitch] is called with midi (NaN when unvoiced) roughly every [chunkMs].
 */
class MicPitch(private val chunkMs: Int = 100) {

    private var job: Job? = null
    @Volatile var pitchMidi: Float = Float.NaN
        private set

    @SuppressLint("MissingPermission")
    fun start(scope: CoroutineScope, onPitch: (Float) -> Unit) {
        stop()
        job = scope.launch(Dispatchers.IO) {
            val rate = 16_000
            val minBuf = AudioRecord.getMinBufferSize(rate, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_FLOAT)
            val bufSize = maxOf(minBuf, rate * chunkMs / 1000 * 4)
            val record = try {
                AudioRecord(MediaRecorder.AudioSource.MIC, rate, AudioFormat.CHANNEL_IN_MONO,
                    AudioFormat.ENCODING_PCM_FLOAT, bufSize)
            } catch (e: Exception) {
                return@launch
            }
            if (record.state != AudioRecord.STATE_INITIALIZED) { record.release(); return@launch }
            record.startRecording()
            val chunkF = rate * chunkMs / 1000
            val buf = FloatArray(chunkF)
            try {
                while (isActive) {
                    val n = record.read(buf, 0, chunkF, AudioRecord.READ_BLOCKING)
                    if (n <= 0) continue
                    val midi = OnKeyCore.nativePitchFrame(buf.copyOf(n))
                    pitchMidi = midi
                    onPitch(midi)
                }
            } finally {
                record.stop()
                record.release()
            }
        }
    }

    fun stop() {
        job?.cancel()
        job = null
    }
}
