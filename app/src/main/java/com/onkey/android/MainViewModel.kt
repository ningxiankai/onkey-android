package com.onkey.android

import android.content.Context
import android.net.Uri
import androidx.lifecycle.ViewModel
import com.onkey.android.learn.LearnJob
import com.onkey.android.native.OnKeyCore
import com.onkey.android.store.LearnedSong
import com.onkey.android.store.SongStore
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import java.io.File

sealed class Nav {
    data object Home : Nav()
    data class Sing(val song: LearnedSong) : Nav()
    data class Score(val song: LearnedSong, val score: Int, val detail: String) : Nav()
}

class MainViewModel : ViewModel() {

    val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private val _nav = MutableStateFlow<Nav>(Nav.Home)
    val nav: StateFlow<Nav> = _nav

    val learn: LearnJob by lazy { LearnJob(appContext!!) }

    private var appContext: Context? = null

    /** Called by MainActivity before UI shows (activity context is fine here). */
    fun init(context: Context) {
        if (appContext == null) appContext = context.applicationContext
    }

    fun library(context: Context): List<LearnedSong> = SongStore.loadAll(context)

    fun onPicked(context: Context, uri: Uri) {
        // copy into our files so it stays valid across restarts
        val name = queryName(context, uri) ?: "song-${System.currentTimeMillis()}.audio"
        val dst = File(context.filesDir, "audio/$name").apply { parentFile?.mkdirs() }
        if (!dst.isFile) {
            context.contentResolver.openInputStream(uri)?.use { input ->
                dst.outputStream().use { output -> input.copyTo(output) }
            }
        }
        val title = name.substringBeforeLast('.').ifBlank { "Untitled" }
        learn.start(dst.absolutePath, title, "", scope)
    }

    fun openSing(song: LearnedSong) {
        _nav.value = Nav.Sing(song)
    }

    fun showScore(song: LearnedSong, score: Int, detail: String) {
        _nav.value = Nav.Score(song, score, detail)
    }

    fun home() {
        _nav.value = Nav.Home
    }

    private fun queryName(context: Context, uri: Uri): String? =
        context.contentResolver.query(uri, null, null, null, null)?.use { c ->
            val idx = c.getColumnIndex(android.provider.OpenableColumns.DISPLAY_NAME)
            if (idx >= 0 && c.moveToFirst()) c.getString(idx) else null
        }
}
