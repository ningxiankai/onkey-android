package com.onkey.android.audio

import android.media.MediaCodec
import android.media.MediaExtractor
import android.media.MediaFormat
import androidx.annotation.NonNull

/**
 * Decode any media file the user picked (SAF uri path) to interleaved stereo f32 at
 * [targetRate] (resampled with a plain linear filter; the pipeline's analyses are not
 * phase-critical). Output: interleaved L/R samples, so samples.size == 2 * frames.
 */
object AudioDecoder {

    fun decode(path: String, targetRate: Int = 48_000): Decoded {
        val extractor = MediaExtractor()
        extractor.setDataSource(path)
        var trackIdx = -1
        var format: MediaFormat? = null
        for (i in 0 until extractor.trackCount) {
            val f = extractor.getTrackFormat(i)
            val mime = f.getString(MediaFormat.KEY_MIME) ?: continue
            if (mime.startsWith("audio/")) {
                trackIdx = i
                format = f
                break
            }
        }
        require(trackIdx >= 0 && format != null) { "no audio track" }
        extractor.selectTrack(trackIdx)
        val mime = format!!.getString(MediaFormat.KEY_MIME)!!
        val codec = MediaCodec.createDecoderByType(mime)
        codec.configure(format, null, null, 0)
        codec.start()

        val srcRate = format.getInteger(MediaFormat.KEY_SAMPLE_RATE)
        val srcChannels = format.getInteger(MediaFormat.KEY_CHANNEL_COUNT)
        val out = ArrayList<Float>(1 shl 20)
        val info = MediaCodec.BufferInfo()
        var inputDone = false
        var outputDone = false

        fun downmix(pcm: ShortArray, frames: Int) {
            // interleave -> mono-average to stereo f32 (1 ch: duplicate; >2: average pairs)
            var i = 0
            while (i < frames) {
                val base = i * srcChannels
                var l = 0f
                var r = 0f
                when (srcChannels) {
                    1 -> { l = pcm[base] / 32768f; r = l }
                    2 -> { l = pcm[base] / 32768f; r = pcm[base + 1] / 32768f }
                    else -> {
                        val half = srcChannels / 2
                        var sl = 0f; var sr = 0f
                        for (c in 0 until half) sl += pcm[base + c]
                        for (c in half until srcChannels) sr += pcm[base + c]
                        l = sl / half / 32768f; r = sr / (srcChannels - half) / 32768f
                    }
                }
                out.add(l); out.add(r)
                i++
            }
        }

        while (!outputDone) {
            if (!inputDone) {
                val inIdx = codec.dequeueInputBuffer(10_000)
                if (inIdx >= 0) {
                    val buf = codec.getInputBuffer(inIdx)!!
                    val n = extractor.readSampleData(buf, 0)
                    if (n < 0) {
                        codec.queueInputBuffer(inIdx, 0, 0, 0, MediaCodec.BUFFER_FLAG_END_OF_STREAM)
                        inputDone = true
                    } else {
                        codec.queueInputBuffer(inIdx, 0, n, extractor.sampleTime, 0)
                        extractor.advance()
                    }
                }
            }
            val outIdx = codec.dequeueOutputBuffer(info, 10_000)
            if (outIdx >= 0) {
                val buf = codec.getOutputBuffer(outIdx)!!
                val frames = info.size / (2 * srcChannels)
                val pcm = ShortArray(frames * srcChannels)
                buf.asShortBuffer().get(pcm)
                downmix(pcm, frames)
                codec.releaseOutputBuffer(outIdx, false)
                if (info.flags and MediaCodec.BUFFER_FLAG_END_OF_STREAM != 0) outputDone = true
            } else if (outIdx == MediaCodec.INFO_TRY_AGAIN_LATER && inputDone) {
                // drain a bit more
            }
        }
        codec.stop(); codec.release(); extractor.release()

        val resampled = if (srcRate == targetRate) out else resampleLinear(out, srcRate, targetRate)
        return Decoded(resampled.toFloatArray(), targetRate)
    }

    /** Linear resample of interleaved stereo. */
    fun resampleLinear(samples: List<Float>, from: Int, to: Int): ArrayList<Float> {
        val ratio = from.toDouble() / to
        val outFrames = (samples.size / 2 / ratio).toInt().coerceAtLeast(0)
        val out = ArrayList<Float>(outFrames * 2)
        var pos = 0.0
        val lastFrame = samples.size / 2 - 1
        for (i in 0 until outFrames) {
            val i0 = pos.toInt().coerceIn(0, lastFrame)
            val i1 = (i0 + 1).coerceAtMost(lastFrame)
            val t = (pos - i0).toFloat()
            val l0 = samples[i0 * 2]; val r0 = samples[i0 * 2 + 1]
            val l1 = samples[i1 * 2]; val r1 = samples[i1 * 2 + 1]
            out.add(l0 + (l1 - l0) * t); out.add(r0 + (r1 - r0) * t)
            pos += ratio
        }
        return out
    }

    class Decoded(@NonNull val samples: FloatArray, val rate: Int)
}
