package com.onkey.android.store

import android.content.Context
import org.json.JSONArray
import org.json.JSONObject
import java.io.File

/** A learned song: the singer's midi line at [fps] frames per second + metadata. */
data class LearnedSong(
    val id: String,
    val title: String,
    val artist: String,
    val filePath: String,
    val fps: Double,
    val midi: FloatArray,
    val periodicity: FloatArray,
) {
    fun toJson(): JSONObject {
        val o = JSONObject()
        o.put("id", id); o.put("title", title); o.put("artist", artist)
        o.put("file", filePath); o.put("fps", fps)
        o.put("midi", JSONArray(midi.map { if (it.isNaN()) JSONObject.NULL else it }))
        o.put("periodicity", JSONArray(periodicity.map { if (it.isNaN()) JSONObject.NULL else it }))
        return o
    }

    companion object {
        fun fromJson(o: JSONObject): LearnedSong = LearnedSong(
            id = o.getString("id"),
            title = o.optString("title", "Untitled"),
            artist = o.optString("artist", ""),
            filePath = o.getString("file"),
            fps = o.getDouble("fps"),
            midi = floatArray(o.optJSONArray("midi")),
            periodicity = floatArray(o.optJSONArray("periodicity")),
        )

        private fun floatArray(a: JSONArray?): FloatArray {
            a ?: return FloatArray(0)
            return FloatArray(a.length()) { i -> if (a.isNull(i)) Float.NaN else a.getDouble(i).toFloat() }
        }
    }
}

/** Tiny JSON-file library of learned songs in filesDir/library.json. */
object SongStore {
    private fun file(context: Context) = File(context.filesDir, "library.json")

    fun loadAll(context: Context): List<LearnedSong> {
        val f = file(context)
        if (!f.isFile) return emptyList()
        return try {
            val arr = JSONArray(f.readText())
            (0 until arr.length()).map { LearnedSong.fromJson(arr.getJSONObject(it)) }
        } catch (e: Exception) {
            emptyList()
        }
    }

    fun save(context: Context, song: LearnedSong) {
        val all = loadAll(context).filterNot { it.id == song.id } + song
        val arr = JSONArray()
        all.forEach { arr.put(it.toJson()) }
        file(context).writeText(arr.toString())
    }

    fun delete(context: Context, id: String) {
        val all = loadAll(context).filterNot { it.id == id }
        val arr = JSONArray()
        all.forEach { arr.put(it.toJson()) }
        file(context).writeText(arr.toString())
    }

    fun newId(): String = "s${System.currentTimeMillis()}"
}
