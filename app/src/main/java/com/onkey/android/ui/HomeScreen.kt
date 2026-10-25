package com.onkey.android.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.onkey.android.MainViewModel
import com.onkey.android.learn.LearnJob
import com.onkey.android.store.LearnedSong

@Composable
fun HomeScreen(viewModel: MainViewModel, onPick: () -> Unit, onSing: (LearnedSong) -> Unit) {
    val context = LocalContext.current
    val phase by viewModel.learn.phase.collectAsStateWithLifecycle()
    val progress by viewModel.learn.progress.collectAsStateWithLifecycle()
    var library by remember { mutableStateOf(viewModel.library(context)) }
    LaunchedEffect(phase) {
        if (phase is LearnJob.Phase.Done) library = viewModel.library(context)
    }

    Scaffold { padding ->
        Column(Modifier.padding(padding).padding(20.dp)) {
            Text("OnKey", style = MaterialTheme.typography.headlineMedium, fontWeight = FontWeight.Medium)
            Text("跟着你喜欢的歌练唱 · 从你的音乐文件学习旋律", style = MaterialTheme.typography.bodySmall)

            Spacer(Modifier.height(16.dp))

            Button(onClick = onPick, modifier = Modifier.fillMaxWidth()) {
                Text("导入歌曲并学习")
            }

            when (val p = phase) {
                is LearnJob.Phase.Decoding -> {
                    Spacer(Modifier.height(12.dp))
                    LinearProgressIndicator(progress = { 0.3f }, modifier = Modifier.fillMaxWidth())
                    Text("解码音频…", style = MaterialTheme.typography.bodySmall)
                }
                is LearnJob.Phase.Learning -> {
                    Spacer(Modifier.height(12.dp))
                    LinearProgressIndicator(progress = { progress }, modifier = Modifier.fillMaxWidth())
                    Text("学习旋律中 ${(progress * 100).toInt()}%（人声分离 + 音高追踪，别锁屏）", style = MaterialTheme.typography.bodySmall)
                }
                is LearnJob.Phase.Done -> {
                    Spacer(Modifier.height(8.dp))
                    Text("已学会《${p.song.title}》", color = MaterialTheme.colorScheme.primary)
                }
                is LearnJob.Phase.Failed -> {
                    Spacer(Modifier.height(8.dp))
                    Text("学习失败：${p.message}", color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall)
                }
                else -> {}
            }

            Spacer(Modifier.height(20.dp))
            Text("曲库", style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Medium)
            Spacer(Modifier.height(8.dp))

            if (library.isEmpty()) {
                Text("还没有学会的歌。导入一首你电脑/手机里的音频文件试试。", style = MaterialTheme.typography.bodySmall)
            } else {
                LazyColumn(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    items(library.asReversed()) { song ->
                        Card(Modifier.fillMaxWidth()) {
                            Row(Modifier.padding(14.dp), verticalAlignment = Alignment.CenterVertically) {
                                Column(Modifier.weight(1f)) {
                                    Text(song.title, fontWeight = FontWeight.Medium)
                                    Text(song.artist.ifBlank { "本地文件" }, style = MaterialTheme.typography.bodySmall)
                                }
                                Button(onClick = { onSing(song) }) { Text("跟唱") }
                                Spacer(Modifier.width(8.dp))
                                TextButton(onClick = {
                                    com.onkey.android.store.SongStore.delete(context, song.id)
                                    library = viewModel.library(context)
                                }) { Text("删") }
                            }
                        }
                    }
                }
            }

            Spacer(Modifier.height(20.dp))
            Text(
                "仅限私人练习：请使用你有权播放的音乐。学习结果保存在本机，不会上传。",
                style = MaterialTheme.typography.labelSmall,
            )
        }
    }
}
