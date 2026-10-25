package com.onkey.android.ui

import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import com.onkey.android.store.LearnedSong

@Composable
fun ScoreScreen(song: LearnedSong, score: Int, detail: String, onAgain: () -> Unit, onHome: () -> Unit) {
    Column(
        Modifier.fillMaxSize().padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        Text("《${song.title}》", style = MaterialTheme.typography.titleMedium)
        Spacer(Modifier.height(20.dp))
        Text("$score", style = MaterialTheme.typography.displayLarge, fontWeight = FontWeight.Medium,
            color = if (score >= 60) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.error)
        Spacer(Modifier.height(12.dp))
        Text(detail, style = MaterialTheme.typography.bodyMedium)
        Spacer(Modifier.height(32.dp))
        Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Button(onClick = onAgain) { Text("再唱一遍") }
            OutlinedButton(onClick = onHome) { Text("回曲库") }
        }
    }
}
