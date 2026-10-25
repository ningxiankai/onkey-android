package com.onkey.android.ui.theme

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color

val Teal = Color(0xFF1D9E75)
val TealDark = Color(0xFF0F6E56)
val TealLight = Color(0xFF9FE1CB)
val Coral = Color(0xFFD85A30)
val Blue = Color(0xFF378ADD)

private val Light = lightColorScheme(primary = Teal, secondary = TealDark, tertiary = Coral)
private val Dark = darkColorScheme(primary = TealLight, secondary = Teal, tertiary = Coral)

@Composable
fun OnKeyTheme(content: @Composable () -> Unit) {
    MaterialTheme(
        colorScheme = if (isSystemInDarkTheme()) Dark else Light,
        content = content,
    )
}
