package com.onkey.android

import android.content.Context
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.viewmodel.CreationExtras

/** Simple ViewModel provider without extra dependencies. */
object MainViewModelHolder {
    fun get(activity: ComponentActivity): MainViewModel =
        ViewModelProvider.create(
            activity.viewModelStore,
            object : ViewModelProvider.Factory {
                override fun <T : ViewModel> create(modelClass: Class<T>): T {
                    @Suppress("UNCHECKED_CAST")
                    return MainViewModel() as T
                }
            },
            CreationExtras.Empty,
        )[MainViewModel::class.java]
}
