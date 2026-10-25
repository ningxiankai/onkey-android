package com.onkey.android

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.runtime.getValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.onkey.android.ui.theme.OnKeyTheme
import com.onkey.android.ui.HomeScreen
import com.onkey.android.ui.ScoreScreen
import com.onkey.android.ui.SingScreen

class MainActivity : ComponentActivity() {

    private val viewModel: MainViewModel by lazy {
        MainViewModelHolder.get(this)
    }

    private val pickAudio = registerForActivityResult(ActivityResultContracts.OpenDocument()) { uri ->
        if (uri != null) viewModel.onPicked(this, uri)
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        com.onkey.android.native.OnKeyCore.ensureModels(this)
        com.onkey.android.native.OnKeyCore.nativeInit(filesDir.absolutePath)
        viewModel.init(this)

        setContent {
            OnKeyTheme {
                val nav by viewModel.nav.collectAsStateWithLifecycle()
                when (val s = nav) {
                    is Nav.Home -> HomeScreen(
                        viewModel = viewModel,
                        onPick = { pickAudio.launch(arrayOf("audio/*")) },
                        onSing = { song -> viewModel.openSing(song) },
                    )
                    is Nav.Sing -> SingScreen(
                        song = s.song,
                        onDone = { score, detail -> viewModel.showScore(s.song, score, detail) },
                        onBack = { viewModel.home() },
                    )
                    is Nav.Score -> ScoreScreen(
                        song = s.song, score = s.score, detail = s.detail,
                        onAgain = { viewModel.openSing(s.song) },
                        onHome = { viewModel.home() },
                    )
                }
            }
        }
    }
}
