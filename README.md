# OnKey for Android

跟着你喜欢的歌练唱：导入一首你手机里的音频文件，App 学会歌手的旋律（人声分离 + 音高追踪），之后你跟唱时，你的音高实时画在旋律线上，唱完给逐帧评分。

核心算法管线复用自开源项目 [OnKey](https://github.com/dsmurrell/onkey)（macOS 版），MIT 许可。

## 构建（云端，本地零安装）

工程自带 GitHub Actions 工作流（`.github/workflows/build.yml`），推送到 GitHub 后自动构建 APK：

1. `models` job（macOS runner）：下载 OnKey 官方 Release 的 DMG，提取内置的 ONNX 模型并校验 SHA-256；
2. `build` job（Ubuntu runner）：导出 CREPE-tiny、`cargo-ndk` 编译 Rust 核心为 arm64 `.so`、Gradle 打出 APK；
3. 产物 `onkey-debug-apk` 从 Actions 页面下载，传到手机安装（需允许安装未知来源应用）。

本地不需要 JDK / Android SDK / Rust。

## 工程结构

```
core/                    Rust 库（cdylib → libonkey_core.so）
  src/                   上游算法模块（pitch/mdx/game/dsp/…，34 个文件，打 Android 补丁）
  src/engine.rs          替代上游 Tauri 引擎的轻量 shim（log/write_wav/quitting）
  src/capture.rs         macOS 音频采集的 stub（MVP 从文件学习，不做实时采集）
  src/lib.rs             JNI 入口：nativeInit / nativeLearnSong / nativePitchFrame
  models/                crepe-tiny.onnx（CI 导出，编译期 include_bytes）
app/                     Kotlin + Jetpack Compose
  audio/AudioDecoder     MediaCodec 解码任意音频 → 立体声 f32
  audio/MicPitch         AudioRecord 16kHz → 实时音高（JNI → CREPE-tiny）
  learn/LearnJob         学习编排（解码 → 分离+追踪 → 存曲库）
  store/SongStore        已学歌曲的 JSON 曲库（本机 filesDir）
  ui/SingScreen          音符条滚动 + 实时音高线（Canvas）+ 逐帧评分
.github/workflows        云端构建
scripts/export_crepe.py  上游模型导出脚本（MIT）
```

## 与上游 OnKey 的差异

- macOS 系统音频采集（Core Audio）→ MVP 从用户选择的文件学习；Android 10+ 的
  `AudioPlaybackCapture` 实时捕获为二期；
- Tauri 事件循环 → Kotlin 直接驱动 `Learner` 管线；
- Core ML / App Kit / NSLocale 调用全部 cfg 隔离或替换（`ANDROID PATCH` 注释）；
- 不含 Shazam 识别、Spotify/Apple Music 控制、歌词对齐（LRCLIB）。

## 许可与合规

- 上游代码与模型：MIT（Copyright (c) 2026 Daniel Murrell）。分发时保留其许可声明，
  且不得使用 "OnKey" 名称与图标作为产品名；
- UVR MDX-Net 模型要求署名：*vocal separation by the UVR-MDX-NET Karaoke 2 model
  from Ultimate Vocal Remover (Anjok07, aufr33)* —— 需出现在应用"关于"页；
- 仅限私人练习：请使用你有权播放的音乐；学习到的旋律数据只保存在本机，不分发、
  不上传（与上游使用约定一致）。

## 已知限制（MVP）

- 仅 arm64-v8a；APK 较大（内置两个模型，约 200MB+）；
- 评分按帧对比（同一时钟），暂无逐句/歌词视图（GAME 音符模型与歌词对齐在曲库中未启用）；
- 首次学习一首 4 分钟的歌在旗舰机上约需 1-3 分钟（UVR 分离），期间请保持前台。
