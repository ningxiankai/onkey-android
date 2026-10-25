package com.onkey.android.ui

import android.media.MediaPlayer
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import com.onkey.android.audio.MicPitch
import com.onkey.android.store.LearnedSong
import kotlinx.coroutines.delay

/**
 * The singing lane: the singer's midi line scrolls right-to-left through a playhead;
 * your live pitch is drawn over it (green when close, coral when off).
 */
@Composable
fun SingScreen(song: LearnedSong, onDone: (Int, String) -> Unit, onBack: () -> Unit) {
    val context = LocalContext.current
    val player = remember {
        MediaPlayer().apply { setDataSource(song.filePath); isLooping = false }
    }
    val mic = remember { MicPitch(chunkMs = 60) }
    val scope = androidx.compose.runtime.rememberCoroutineScope()

    // user pitch history: (seconds into song, midi)
    val history = remember { mutableStateListOf<Pair<Float, Float>>() }
    var positionMs by remember { mutableIntStateOf(0) }
    var running by remember { mutableStateOf(false) }
    var finished by remember { mutableStateOf(false) }

    DisposableEffect(Unit) {
        player.prepare()
        onDispose {
            mic.stop()
            try { player.stop(); player.release() } catch (_: Exception) {}
        }
    }

    fun finish() {
        if (finished) return
        finished = true
        mic.stop()
        try { player.pause() } catch (_: Exception) {}
        val (score, detail) = score(history.toList(), song)
        onDone(score, detail)
    }

    Column(Modifier.fillMaxSize().padding(16.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            TextButton(onClick = { finish(); onBack() }) { Text("返回") }
            Spacer(Modifier.weight(1f))
            Text("${song.title}", style = MaterialTheme.typography.titleMedium)
            Spacer(Modifier.weight(1f))
            Text("%d:%02d".format(positionMs / 60000, positionMs / 1000 % 60), style = MaterialTheme.typography.bodySmall)
        }

        Spacer(Modifier.height(12.dp))

        Canvas(Modifier.fillMaxWidth().height(260.dp)) {
            with(this) {
                drawLane(size.width.toInt(), size.height.toInt(), song, positionMs / 1000f, history.toList())
            }
        }

        Spacer(Modifier.height(16.dp))

        Row(horizontalArrangement = Arrangement.spacedBy(12.dp), modifier = Modifier.align(Alignment.CenterHorizontally)) {
            if (!running) {
                Button(onClick = {
                    running = true
                    mic.start(scope) { midi ->
                        val t = positionMs / 1000f
                        if (!midi.isNaN()) history.add(t to midi)
                    }
                    player.start()
                }) { Text("开始跟唱") }
            } else {
                Button(onClick = { finish() }) { Text("结束并评分") }
            }
        }

        Spacer(Modifier.height(8.dp))
        Text(
            "戴上耳机效果最好（唱进麦克风的才只有你的声音）",
            style = MaterialTheme.typography.labelSmall,
            modifier = Modifier.align(Alignment.CenterHorizontally),
        )
    }

    // song progress ticker
    LaunchedEffect(running) {
        while (running && !finished) {
            positionMs = try { player.currentPosition } catch (e: Exception) { positionMs }
            if (!player.isPlaying) {
                delay(200)
                if (!player.isPlaying && positionMs > 1000) { finish() }
            }
            delay(60)
        }
    }
}

/** midi -> lane y (lane spans C3..C6). */
private fun midiToY(midi: Float, height: Int): Float {
    val top = 84f   // C6
    val bottom = 48f // C3
    val frac = ((top - midi) / (top - bottom)).coerceIn(0f, 1f)
    return height * (0.06f + 0.88f * frac)
}

private fun androidx.compose.ui.graphics.drawscope.DrawScope.drawLane(
    width: Int,
    height: Int,
    song: LearnedSong,
    nowSec: Float,
    history: List<Pair<Float, Float>>,
) {
    val w = width.toFloat()
    val h = height.toFloat()
    val windowSec = 5f // seconds visible ahead

    // lane guides at whole notes (C, E, G)
    for (m in 48..84 step 4) {
        val y = midiToY(m.toFloat(), height)
        drawLine(Color.Gray.copy(alpha = 0.15f), Offset(0f, y), Offset(w, y), 1f)
    }

    // playhead at 1/4 from the left
    val playX = w * 0.25f
    drawLine(Color.Black.copy(alpha = 0.4f), Offset(playX, 0f), Offset(playX, h), 2f)

    // singer's line: frames at song.fps; a frame at time t appears at x = playX + (t - now) / windowSec * (w - playX)... actually ahead of playhead toward the right
    val fps = song.fps.toFloat()
    val path = Path()
    var pen = false
    val lastFrame = song.midi.size - 1
    var i = ((nowSec - 1f) * fps).toInt().coerceIn(0, lastFrame)
    val endFrame = ((nowSec + windowSec) * fps).toInt().coerceAtMost(lastFrame)
    while (i <= endFrame) {
        val midi = song.midi[i]
        val t = i / fps
        if (midi.isNaN()) { pen = false } else {
            val x = playX + (t - nowSec) / windowSec * (w - playX)
            val y = midiToY(midi, height)
            if (!pen) { path.moveTo(x, y); pen = true } else path.lineTo(x, y)
        }
        i++
    }
    drawPath(path, Color(0xFF1D9E75), style = Stroke(width = 6f))

    // your pitch: coral over the green singer's line
    val hist = history
    var hi = hist.size - 1
    while (hi >= 0 && hist[hi].first > nowSec) hi--   // drop samples ahead of the playhead (clock jitter)
    var start = hi
    while (start >= 0 && hist[start].first > nowSec - 1.2f) start--
    var pen2 = false
    val upath = Path()
    for (k in maxOf(start, 0)..hi) {
        val (t, midi) = hist[k]
        if (midi.isNaN()) { pen2 = false; continue }
        val x = playX + (t - nowSec) / windowSec * (w - playX)
        if (x < 0) { pen2 = false; continue }
        val y = midiToY(midi, height)
        if (!pen2) { upath.moveTo(x, y); pen2 = true } else upath.lineTo(x, y)
    }
    drawPath(upath, Color(0xFFD85A30), style = Stroke(width = 4f))
}

/**
 * Frame-wise scoring: for every user sample, compare to the singer's midi at the same
 * time (skip frames where the song is unvoiced). Cents deviation -> score.
 */
fun score(history: List<Pair<Float, Float>>, song: LearnedSong): Pair<Int, String> {
    if (history.isEmpty()) return 0 to "没有听到你的声音"
    val fps = song.fps.toFloat()
    var voiced = 0
    var totalCents = 0.0
    var worst = 0.0
    for ((t, midi) in history) {
        if (midi.isNaN()) continue
        val frame = (t * fps).toInt()
        if (frame < 0 || frame >= song.midi.size) continue
        val target = song.midi[frame]
        if (target.isNaN()) continue
        val cents = Math.abs(1200.0 * Math.log((midi / target).toDouble()) / Math.log(2.0))
        voiced++
        totalCents += cents
        if (cents > worst) worst = cents
    }
    if (voiced == 0) return 0 to "没有对上旋律的帧"
    val avg = totalCents / voiced
    // 0 cents ~ 100, 50 cents ~ 60, 200 cents ~ 0
    val s = (100 - (avg / 50.0) * 40).coerceIn(0.0, 100.0)
    val verdict = when {
        avg < 30 -> "音准很稳"
        avg < 60 -> "大体在线上，个别句子飘了"
        else -> "多听原曲，跟着绿色线条练"
    }
    return s.toInt() to "$verdict（平均偏差 ${"%.0f".format(avg)} 音分，最大 ${"%.0f".format(worst)}，采样 $voiced 帧）"
}
