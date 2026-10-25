//! What OnKey keeps on disk, under the app's data folder:
//!   songs/<track>.bin + .json   a learned song: the singer's pitch line, the rhythm fingerprint and which
//!                               10 ms frames have been learned (never audio)
//!   songs/<track>.words       when each lyric word was heard sung when it was learned (times and places in the
//!                               lyrics, never the words), for checking a play's lyrics against the singing
//!   songs/<track>.shsig         its Shazam signature (a compact fingerprint, not audio), so listening mode
//!                               recognises it on this Mac with no network
//!   takes/<time>-<song>/        one sung take: mic.m4a (your voice, AAC; mic.wav before: takeaudio.rs), result.json (scores), meta.json
//!   favorites-pending.json      hearts given to songs still being learned (no songs/<track>.json yet), moved
//!                               into the song's .json ("favorite", "favorited_at") once it's saved
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::dsp::{locate, FP_BANDS, FRAME_S};
use crate::source::{Source, Track};

/// A song is ready to sing once this much of it has been learned.
pub const READY_COVERAGE: f64 = 0.7;

#[derive(Clone, Default)]
pub struct Learned {
    pub midi: Vec<f32>,
    pub covered: Vec<bool>,
    pub onset: Vec<[f32; FP_BANDS]>,
}

impl Learned {
    pub fn fraction(&self, duration: f64) -> f64 {
        self.covered.iter().filter(|c| **c).count() as f64 * FRAME_S / duration.max(1.0)
    }
    pub fn ready(&self, duration: f64) -> bool {
        !self.onset.is_empty() && self.fraction(duration) >= READY_COVERAGE
    }
}

#[derive(Clone)]
pub struct Store {
    pub root: PathBuf,
}

const MAGIC: &[u8; 4] = b"OKS1";

fn safe(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect()
}

/// A take's id as the store names them (`new_take_dir`: "20261015-120000-Adele-Hello", "…-2"): letters,
/// digits, '-', '_' and '.', starting with a letter or digit and never "..". So never "", "." or "..", a hidden
/// name or a path: an id from the window can't reach outside takes/ (delete_take("..") once resolved to the data
/// folder itself, and a failed move to the Trash fell through to deleting it all).
pub fn valid_take_id(id: &str) -> bool {
    id.len() <= 255
        && id.starts_with(|c: char| c.is_ascii_alphanumeric())
        && id.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
        && !id.contains("..")
}

/// A song id from the window: something with a letter or digit in it (its file name is `safe`, letters, digits
/// and '_' only, so it can't be a path whatever it is; this only keeps out ids that would name no song at all).
pub fn valid_song_id(id: &str) -> bool {
    id.len() <= 255 && id.chars().any(|c| c.is_ascii_alphanumeric())
}

/// Write a file so that whoever reads it next (or a crash, a kill, a power cut mid-write) sees the old file or
/// the new one, never part of one: written to a temp file beside it, named for this writer alone (process and
/// a counter, so two writers never share one), flushed to disk, then renamed over it. Every file OnKey keeps
/// is written this way.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = temp_beside(path)?;
    let written = (|| {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

/// A temp file's name beside `path` for this writer alone (".<file>.tmp-<pid>-<n>"), for writing it atomically.
pub(crate) fn temp_beside(path: &Path) -> std::io::Result<PathBuf> {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = path.file_name().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name"))?;
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    Ok(path.with_file_name(format!(".{}{TMP_MARK}{}-{n}", name.to_string_lossy(), std::process::id())))
}

/// One writer at a time for the store's read-change-write updates (a song's .bin and .json,
/// favorites-pending.json): two takes of a song finishing together would each read the old song, add their part
/// and write, and the last write would drop the other's learning; the cover finder, a heart from the window and
/// a score thread likewise. Store-wide (every Store on any folder: they're cheap to make and cloned freely) and
/// re-entrant, since `canonical` → `merge_into` → `save_song` take it on one thread.
struct WriteLock {
    owner: std::sync::Mutex<(Option<std::thread::ThreadId>, usize)>,
    free: std::sync::Condvar,
}

static WRITES: WriteLock = WriteLock { owner: std::sync::Mutex::new((None, 0)), free: std::sync::Condvar::new() };

/// Held while one thread updates the store (see `WriteLock`).
struct Writing;

fn writing() -> Writing {
    let me = std::thread::current().id();
    let mut o = WRITES.owner.lock().unwrap_or_else(|e| e.into_inner());
    while o.0.is_some_and(|t| t != me) {
        o = WRITES.free.wait(o).unwrap_or_else(|e| e.into_inner());
    }
    o.0 = Some(me);
    o.1 += 1;
    Writing
}

impl Drop for Writing {
    fn drop(&mut self) {
        let mut o = WRITES.owner.lock().unwrap_or_else(|e| e.into_inner());
        o.1 -= 1;
        if o.1 == 0 {
            o.0 = None;
            WRITES.free.notify_one();
        }
    }
}

/// In the name of `write_atomic`'s temp files: ".<file>.tmp-<pid>-<n>".
pub(crate) const TMP_MARK: &str = ".tmp-";

/// Temp files left in `dir` by a write that never finished (OnKey was killed mid-write): another process's, and
/// over ten minutes old (so a lab tool opening the same folder never takes one from under a running OnKey).
fn sweep_temp_files(dir: &Path) {
    let mine = format!("{TMP_MARK}{}-", std::process::id());
    let old = |e: &std::fs::DirEntry| e.metadata().ok().and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() > 600);
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let left = (name.starts_with('.') && name.contains(TMP_MARK) && !name.contains(&mine)) || name.ends_with(".bin.tmp");
        if left && e.path().is_file() && old(&e) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

impl Store {
    pub fn new(root: PathBuf) -> Store {
        let _ = std::fs::create_dir_all(root.join("songs"));
        let _ = std::fs::create_dir_all(root.join("takes"));
        sweep_temp_files(&root.join("songs"));
        sweep_temp_files(&root);
        Store { root }
    }

    /// A new take's folder, takes/<name>; "<name>-2", "-3", … when a take of the same song was saved in the same
    /// second (two takes finishing together), so one never overwrites the other. Returns it and its name.
    pub fn new_take_dir(&self, name: &str) -> std::io::Result<(PathBuf, String)> {
        std::fs::create_dir_all(self.takes_dir())?;
        for k in 1..1000 {
            let n = if k == 1 { name.to_string() } else { format!("{name}-{k}") };
            let d = self.takes_dir().join(&n);
            match std::fs::create_dir(&d) {
                Ok(()) => return Ok((d, n)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free take folder name"))
    }

    fn song_path(&self, id: &str, ext: &str) -> PathBuf {
        self.root.join("songs").join(format!("{}.{ext}", safe(id)))
    }

    pub fn takes_dir(&self) -> PathBuf {
        self.root.join("takes")
    }

    pub fn load_song(&self, id: &str) -> Option<Learned> {
        let mut b = Vec::new();
        std::fs::File::open(self.song_path(id, "bin")).ok()?.read_to_end(&mut b).ok()?;
        if b.len() < 8 || &b[..4] != MAGIC {
            return None;
        }
        let n = u32::from_le_bytes(b[4..8].try_into().ok()?) as usize;
        if b.len() < 8 + n * (4 + 1 + 2 * FP_BANDS) {
            return None;
        }
        let mut p = 8;
        let midi = (0..n).map(|i| f32::from_le_bytes(b[p + 4 * i..p + 4 * i + 4].try_into().unwrap())).collect();
        p += 4 * n;
        let covered = b[p..p + n].iter().map(|v| *v != 0).collect();
        p += n;
        let onset = (0..n).map(|i| {
            let mut row = [0f32; FP_BANDS];
            for (k, r) in row.iter_mut().enumerate() {
                let o = p + 2 * (i * FP_BANDS + k);
                *r = i16::from_le_bytes([b[o], b[o + 1]]) as f32 / 256.0;
            }
            row
        }).collect();
        let l = Learned { midi, covered, onset };
        // a song saved without a fingerprint has all-zero rows; treat it as having none
        Some(if l.onset.iter().all(|r| r.iter().all(|v| *v == 0.0)) { Learned { onset: Vec::new(), ..l } } else { l })
    }

    fn write_song(&self, id: &str, l: &Learned) -> std::io::Result<()> {
        let n = l.midi.len();
        let mut b = Vec::with_capacity(8 + n * (5 + 2 * FP_BANDS));
        b.extend_from_slice(MAGIC);
        b.extend_from_slice(&(n as u32).to_le_bytes());
        for v in &l.midi {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend(l.covered.iter().map(|c| *c as u8));
        for i in 0..n {
            let row = l.onset.get(i).copied().unwrap_or([0.0; FP_BANDS]);
            for v in row {
                b.extend_from_slice(&((v * 256.0).round().clamp(-32768.0, 32767.0) as i16).to_le_bytes());
            }
        }
        write_atomic(&self.song_path(id, "bin"), &b)
    }

    /// Merge a take's singer line + fingerprint (song-time frames) into the song. If the song is already
    /// partly learned, the new part is first lined up with it by fingerprint (start times can be off).
    /// Returns false when the new part was rejected: it overlaps what's learned but its fingerprint doesn't
    /// confirm where it belongs (merging it would overwrite good data with misplaced notes), or it couldn't be
    /// written. Load, merge and write happen under the store's write lock, so two takes of the song finishing
    /// together are both kept.
    pub fn save_song(&self, track: &Track, midi: Vec<f32>, covered: Vec<bool>, onset: Vec<[f32; FP_BANDS]>) -> bool {
        self.save_song_noted(track, midi, covered, onset, (false, false))
    }

    /// save_song, for a play whose length is a guess and/or that was a YouTube video (`note`; see keep_known).
    fn save_song_noted(&self, track: &Track, mut midi: Vec<f32>, mut covered: Vec<bool>, mut onset: Vec<[f32; FP_BANDS]>,
                       note: (bool, bool)) -> bool {
        let _writing = writing();
        let old = self.load_song(&track.id);
        if let Some(old) = &old {
            if !old.onset.is_empty() && !onset.is_empty() {
                let Some(sh) = fingerprint_shift(&onset, &old.onset, &covered, &old.covered) else { return false };
                if sh != 0 {
                    midi = shift(&midi, sh, f32::NAN);
                    covered = shift(&covered, sh, false);
                    onset = shift(&onset, sh, [0.0; FP_BANDS]);
                }
            }
        }
        let merged = match old {
            Some(old) => {
                let n = old.midi.len().max(midi.len());
                let mut m = fit(&old.midi, n, f32::NAN);
                let mut cov = fit(&old.covered, n, false);
                let mut env = fit(&old.onset, n, [0.0; FP_BANDS]);
                let new_cov = fit(&covered, n, false);
                let new_m = fit(&midi, n, f32::NAN);
                let new_env = fit(&onset, n, [0.0; FP_BANDS]);
                for i in 0..n {
                    if new_cov[i] {
                        m[i] = new_m[i];
                        if !onset.is_empty() {
                            env[i] = new_env[i];
                        }
                        cov[i] = true;
                    }
                }
                Learned { midi: m, covered: cov, onset: env }
            }
            None => Learned { midi, covered, onset },
        };
        if let Err(e) = self.write_song(&track.id, &merged) {
            if !cfg!(test) {
                crate::engine::log(&format!("{} - {}: couldn't be saved: {e}", track.artist, track.name));
            }
            return false;
        }
        {
            let mut track = track.clone();
            // keep a cover found earlier, and the Apple Music page found for the same cover
            if let Some(c) = self.cover(&track.id).filter(|c| track.art.is_none() || track.art.as_ref() == Some(&c.art)) {
                track.art = Some(c.art);
                track.link = track.link.or(c.link);
            }
            // what the song already knows about itself isn't lost to a play that knew less (see keep_known)
            let (mut guessed, youtube) = note;
            if let Some(m) = self.song_meta(&track.id) {
                if let Ok(old) = serde_json::from_value::<Track>(m["track"].clone()) {
                    (track, guessed) = keep_known(&old, m["length_guessed"].as_bool().unwrap_or(false), track, guessed, youtube);
                }
            }
            let mut meta = json!({"track": track, "learned": now_iso(), "fraction": merged.fraction(track.duration)});
            if guessed {
                meta["length_guessed"] = json!(true);
            }
            if let Some(old) = self.song_meta(&track.id) {
                // keep lyrics already lined up with this song, and its heart
                for k in ["lyric_times", "lyric_check", "lyric_lines", "lyrics", "lyrics_placed", "favorite", "favorited_at", "played_in", "aliases"] {
                    if !old[k].is_null() {
                        meta[k] = old[k].clone();
                    }
                }
            }
            // hearted while it was being learned
            if let Some(at) = self.take_pending_favorite(&track.id) {
                mark_favorite(&mut meta, true, &at);
            }
            let _ = write_atomic(&self.song_path(&track.id, "json"), meta.to_string().as_bytes());
        }
        true
    }

    /// Change a song's .json with `f` (which says whether it changed anything); written only if it did.
    fn with_meta(&self, id: &str, f: impl FnOnce(&mut Value) -> bool) {
        let _writing = writing();
        if let Some(mut m) = self.song_meta(id) {
            if f(&mut m) {
                let _ = write_atomic(&self.song_path(id, "json"), m.to_string().as_bytes());
            }
        }
    }

    /// A song kept as `id` was played (sung or learned) in `app`, as `played_as` there (Spotify's track id, Apple
    /// Music's, a YouTube video's; or the song's own id): kept with it, so it's opened there again.
    pub fn note_play(&self, id: &str, played_as: &str, app: Source) {
        self.with_meta(id, |m| {
            let mut changed = add_alias(m, id, played_as);
            if let Some(k) = app_key(app) {
                changed |= m["played_in"].as_str() != Some(k);
                m["played_in"] = json!(k);
            }
            changed
        });
    }

    /// The song kept as `id` is `alias` in an app (its Spotify track id, say).
    pub fn note_alias(&self, id: &str, alias: &str) {
        self.with_meta(id, |m| add_alias(m, id, alias));
    }

    /// The app you usually sing from: the one with the most takes in History (None when no take says).
    pub fn usual_app(&self) -> Option<Source> {
        usual_app(&self.takes())
    }

    /// Where to open the song kept as `id` (or known by it in an app) in your music app; `running`: the app
    /// being listened to now, if any. With the song's track as stored, when it's learned here.
    pub fn open_pick(&self, id: &str, running: Option<Source>) -> (Pick, Option<Track>) {
        let m = self.song_meta(id).or_else(|| self.learned_metas().into_iter().find(|m| aliases(m).iter().any(|a| a == id)));
        let own = m.as_ref().and_then(|m| m["track"]["id"].as_str().map(String::from)).unwrap_or_else(|| id.to_string());
        let ids: Vec<String> = m.as_ref().map(aliases).unwrap_or_default();
        let played_in = m.as_ref().and_then(|m| m["played_in"].as_str().and_then(app_of_key));
        let track = m.and_then(|m| serde_json::from_value::<Track>(m["track"].clone()).ok());
        (pick_app(&own, &ids, played_in, self.usual_app(), running), track)
    }

    fn learned_metas(&self) -> Vec<Value> {
        std::fs::read_dir(self.root.join("songs")).into_iter().flatten().flatten()
            .filter(|e| is_song_meta(&e.path()))
            .filter_map(|e| serde_json::from_slice::<Value>(&std::fs::read(e.path()).ok()?).ok())
            .filter(|m| m["track"]["id"].is_string())
            .collect()
    }

    /// Once at start-up: the apps' ids and the app played in, for songs learned before they were kept: from the
    /// entries merged into them (songs/merged/) and from your takes (their track ids, newest first).
    pub fn backfill_sources(&self) -> usize {
        let learned = self.learned_tracks();
        let find = |t: &Track| learned.iter().find(|k| k.id == t.id).or_else(|| learned.iter().find(|k| same_title(k, t))).map(|k| k.id.clone());
        let mut found: Vec<(String, String, Option<Source>)> = Vec::new();   // (song id, id played as, app)
        for e in std::fs::read_dir(self.root.join("songs").join("merged")).into_iter().flatten().flatten() {
            let t = std::fs::read(e.path()).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .and_then(|m| serde_json::from_value::<Track>(m["track"].clone()).ok());
            if let Some((t, id)) = t.and_then(|t| find(&t).map(|id| (t, id))) {
                found.push((id, t.id, None));
            }
        }
        for t in self.takes() {
            let Ok(track) = serde_json::from_value::<Track>(t["track"].clone()) else { continue };
            let app = t["app"].as_str().and_then(app_of_key).or(Some(Source::of_id(&track.id))).filter(|a| *a != Source::Other);
            if let Some(id) = find(&track) {
                found.push((id, track.id, app));
            }
        }
        let mut changed = 0;
        for id in learned.iter().map(|k| &k.id) {
            let mine: Vec<&(String, String, Option<Source>)> = found.iter().filter(|f| f.0 == *id).collect();
            if mine.is_empty() {
                continue;
            }
            self.with_meta(id, |m| {
                let mut c = mine.iter().rev().fold(false, |c, f| add_alias(m, id, &f.1) | c);   // the newest first
                if m["played_in"].is_null() {
                    if let Some(k) = mine.iter().find_map(|f| f.2).and_then(app_key) {
                        m["played_in"] = json!(k);
                        c = true;
                    }
                }
                changed += c as usize;
                c
            });
        }
        changed
    }

    /// The id a song's heart is kept under: its own when it's learned under it, else the learned song with its
    /// artist and title (a Spotify id for a song kept by its ISRC), else its own (a song still being learned).
    fn favorite_id(&self, t: &Track) -> String {
        if t.name.is_empty() || self.song_path(&t.id, "json").exists() {
            return t.id.clone();
        }
        self.known_as(t).map_or_else(|| t.id.clone(), |k| k.id)
    }

    /// Heart a song (or take its heart away). Kept in the song's .json, so it goes wherever the song goes; a song
    /// not saved yet (being learned for the first time) gets it once it's saved. Returns whether it's a favourite.
    pub fn set_favorite(&self, t: &Track, on: bool) -> bool {
        let _writing = writing();
        let id = self.favorite_id(t);
        if let Some(mut m) = self.song_meta(&id) {
            if m["favorite"].as_bool().unwrap_or(false) != on {
                mark_favorite(&mut m, on, &now_iso());
                let _ = write_atomic(&self.song_path(&id, "json"), m.to_string().as_bytes());
            }
        } else {
            let mut p = self.pending_favorites();
            if on {
                p.entry(id).or_insert_with(|| json!(now_iso()));
            } else {
                p.remove(&id);
            }
            self.write_pending_favorites(&p);
        }
        on
    }

    /// The song a track's takes belong to, as its heart is kept (`favorite_id`): a Spotify id and the ISRC
    /// it's learned under are one song.
    pub fn song_key(&self, t: &Track) -> String {
        self.favorite_id(t)
    }

    pub fn is_favorite(&self, t: &Track) -> bool {
        let id = self.favorite_id(t);
        match self.song_meta(&id) {
            Some(m) => m["favorite"].as_bool().unwrap_or(false),
            None => self.pending_favorites().contains_key(&id),
        }
    }

    fn pending_favorites(&self) -> serde_json::Map<String, Value> {
        std::fs::read(self.root.join("favorites-pending.json")).ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v.as_object().cloned()).unwrap_or_default()
    }

    fn write_pending_favorites(&self, p: &serde_json::Map<String, Value>) {
        let path = self.root.join("favorites-pending.json");
        if p.is_empty() {
            let _ = std::fs::remove_file(path);
        } else {
            let _ = write_atomic(&path, Value::Object(p.clone()).to_string().as_bytes());
        }
    }

    /// A heart given to `id` before it was saved: when (and it's no longer pending).
    fn take_pending_favorite(&self, id: &str) -> Option<String> {
        let _writing = writing();
        let mut p = self.pending_favorites();
        let at = p.remove(id)?;
        self.write_pending_favorites(&p);
        Some(at.as_str().map_or_else(now_iso, String::from))
    }

    /// Whether a learned song needs (more) syllable bars: none, or too few to show (see `thin_bars`).
    pub fn needs_bars(&self, id: &str, duration: f64) -> bool {
        let span = match self.kept_lyrics(id) {
            Some(KeptLyrics::Times(k)) => k.times.iter().copied().fold(None, |s: Option<(f64, f64)>, t| Some(s.map_or((t, t), |(a, b)| (a.min(t), b.max(t))))),
            _ => None,
        };
        thin_bars(self.syllables(id, None).as_deref().unwrap_or(&[]), duration, span)
    }

    /// Whether a play has placed the song's lyrics by the words heard (so its times are no longer LRCLIB's).
    pub fn lyrics_placed(&self, id: &str) -> bool {
        self.song_meta(id).is_some_and(|m| m["lyrics_placed"].as_bool() == Some(true))
    }

    /// A play placed the song's lyrics by the words heard, believed (see lyrics_placed).
    pub fn mark_lyrics_placed(&self, id: &str) {
        self.with_meta(id, |m| {
            let changed = m["lyrics_placed"].as_bool() != Some(true);
            m["lyrics_placed"] = json!(true);
            changed
        });
    }

    /// A learned song's length as kept (s), when it has one.
    pub fn song_duration(&self, id: &str) -> Option<f64> {
        self.song_meta(id)?["track"]["duration"].as_f64().filter(|d| *d > 0.0)
    }

    fn song_meta(&self, id: &str) -> Option<Value> {
        serde_json::from_slice(&std::fs::read(self.song_path(id, "json")).ok()?).ok()
    }

    /// Forget a learned song (to the Trash, so it can be recovered); your takes stay.
    pub fn forget(&self, id: &str) {
        if !valid_song_id(id) {
            return;
        }
        let _writing = writing();
        for ext in ["bin", "json", "syl.json", "shsig", "words"] {
            to_trash(&self.song_path(id, ext));
        }
        crate::lyrics::forget_learned(id);
    }

    /// Keep a song's Shazam signature, made from `seconds` of its audio starting `start` s into the song. A
    /// longer one replaces a shorter one (a later, fuller play of the song).
    pub fn save_signature(&self, id: &str, sig: &[u8], start: f64, seconds: f64) {
        let _writing = writing();
        if self.signature(id).is_some_and(|(_, _, had)| had >= seconds) {
            return;
        }
        let mut b = Vec::with_capacity(sig.len() + 20);
        b.extend_from_slice(b"SHS1");
        b.extend_from_slice(&start.to_le_bytes());
        b.extend_from_slice(&seconds.to_le_bytes());
        b.extend_from_slice(sig);
        let _ = write_atomic(&self.song_path(id, "shsig"), &b);
    }

    /// A song's Shazam signature: (bytes, song seconds where it starts, seconds it covers).
    pub fn signature(&self, id: &str) -> Option<(Vec<u8>, f64, f64)> {
        let b = std::fs::read(self.song_path(id, "shsig")).ok()?;
        if b.len() < 20 || &b[..4] != b"SHS1" {
            return None;
        }
        let start = f64::from_le_bytes(b[4..12].try_into().ok()?);
        let seconds = f64::from_le_bytes(b[12..20].try_into().ok()?);
        Some((b[20..].to_vec(), start, seconds))
    }

    /// The song learned here that `t` is, when it's kept under another id (Spotify's track id for a song learned
    /// from Shazam's match, by its ISRC, say): the same artist and title (letters and digits, any case). None when
    /// `t` is kept under its own id, or isn't learned. Another version ("Castle on the Hill - Acoustic") is
    /// another song.
    pub fn known_as(&self, t: &Track) -> Option<Track> {
        if self.song_path(&t.id, "json").exists() {
            return None;
        }
        self.learned_tracks().into_iter().find(|k| same_title(k, t))
    }

    /// The id song `t` is kept under: one entry per song, by its ISRC when it has one (Shazam's), so a song heard
    /// from Spotify (Spotify's track id), Apple Music or Shazam is one song. Other entries for it (the same artist
    /// and title under other ids) are merged into that one first. Returns `t` with that id.
    pub fn canonical(&self, t: &Track) -> Track {
        let _writing = writing();
        let same: Vec<Track> = self.learned_tracks().into_iter().filter(|k| k.id != t.id && same_title(k, t)).collect();
        if same.is_empty() {
            return t.clone();
        }
        let isrc = |id: &str| id.starts_with("isrc:");
        let id = if isrc(&t.id) {
            t.id.clone()
        } else if let Some(k) = same.iter().find(|k| isrc(&k.id)) {
            k.id.clone()
        } else if self.song_path(&t.id, "json").exists() {
            t.id.clone()
        } else {
            same[0].id.clone()
        };
        let target = Track { id, ..t.clone() };
        let others: Vec<String> = same.iter().map(|k| k.id.clone()).chain(std::iter::once(t.id.clone()))
            .filter(|k| *k != target.id && self.song_path(k, "json").exists()).collect();
        for k in others {
            self.merge_into(&k, &target);
        }
        // a heart given to the song under another id while it was being learned goes with it
        for k in same.iter().map(|k| k.id.as_str()).chain(std::iter::once(t.id.as_str())).filter(|k| *k != target.id) {
            if let Some(at) = self.take_pending_favorite(k) {
                let mut p = self.pending_favorites();
                p.entry(target.id.clone()).or_insert(json!(at));
                self.write_pending_favorites(&p);
            }
        }
        if let Some(mut m) = self.song_meta(&target.id) {
            if let Some(at) = self.take_pending_favorite(&target.id) {
                mark_favorite(&mut m, true, &at);
                let _ = write_atomic(&self.song_path(&target.id, "json"), m.to_string().as_bytes());
            }
        }
        target
    }

    /// Save a play's learning (see `save_song`) under the song's one id (see `canonical`), the two together: plays
    /// of one song saved at the same moment under two ids end up as one entry. Returns whether the play was added,
    /// and the track as kept.
    pub fn save_as_one(&self, track: &Track, midi: Vec<f32>, covered: Vec<bool>, onset: Vec<[f32; FP_BANDS]>) -> (bool, Track) {
        static SAVING: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _one = SAVING.lock().unwrap_or_else(|e| e.into_inner());
        // what this play knew less well, for this save only (not a merge canonical makes on the way)
        let note = take_play_note();
        let canon = self.canonical(track);
        (self.save_song_noted(&canon, midi, covered, onset, note), canon)
    }

    /// Merge the song kept as `from` into `to` (lined up by fingerprint where both have some), and put `from`'s
    /// files aside in songs/merged/.
    fn merge_into(&self, from: &str, to: &Track) {
        let _writing = writing();
        let Some(old) = self.load_song(from) else { return };
        let from_meta = self.song_meta(from);
        let kept = match self.load_song(&to.id) {
            None => {
                // nothing under `to` yet: `from` becomes it
                let _ = self.write_song(&to.id, &old);
                let mut meta = self.song_meta(from).unwrap_or_else(|| json!({}));
                meta["track"] = json!(Track { id: to.id.clone(), ..to.clone() });
                meta["fraction"] = json!(old.fraction(to.duration));
                let _ = write_atomic(&self.song_path(&to.id, "json"), meta.to_string().as_bytes());
                true
            }
            Some(had) => {
                let into = self.song_meta(&to.id).and_then(|m| serde_json::from_value::<Track>(m["track"].clone()).ok()).unwrap_or_else(|| to.clone());
                let merged = self.save_song(&into, old.midi.clone(), old.covered.clone(), old.onset.clone());
                // couldn't be lined up: the one with more of the song learned is kept
                if !merged && old.fraction(to.duration) > had.fraction(to.duration) {
                    let _ = self.write_song(&to.id, &old);
                }
                merged
            }
        };
        // the ids it was known by in the apps, and the app it played in, go with it
        if let Some(f) = &from_meta {
            let mut ids = aliases(f);
            ids.insert(0, from.to_string());
            self.with_meta(&to.id, |m| {
                let mut changed = ids.iter().fold(false, |c, a| add_alias(m, &to.id, a) | c);
                if m["played_in"].is_null() && f["played_in"].is_string() {
                    m["played_in"] = f["played_in"].clone();
                    changed = true;
                }
                changed
            });
        }
        // a favourite stays one if either entry was (hearted the latest time either was)
        if let Some(f) = from_meta.filter(|f| f["favorite"].as_bool() == Some(true)) {
            if let Some(mut m) = self.song_meta(&to.id) {
                let at = f["favorited_at"].as_str().unwrap_or("");
                let had = m["favorite"].as_bool() == Some(true);
                if !had || m["favorited_at"].as_str().unwrap_or("") < at {
                    mark_favorite(&mut m, true, at);
                    let _ = write_atomic(&self.song_path(&to.id, "json"), m.to_string().as_bytes());
                }
            }
        }
        let aside = self.root.join("songs").join("merged");
        let _ = std::fs::create_dir_all(&aside);
        for ext in ["shsig", "syl.json", "words"] {
            if !self.song_path(&to.id, ext).exists() {
                let _ = std::fs::rename(self.song_path(from, ext), self.song_path(&to.id, ext));
            }
        }
        for ext in ["bin", "json", "syl.json", "shsig", "words"] {
            let p = self.song_path(from, ext);
            if p.exists() {
                let _ = std::fs::rename(&p, aside.join(p.file_name().unwrap_or_default()));
            }
        }
        if !cfg!(test) {
            crate::engine::log(&format!("{} - {}: kept as {} was merged into {}{}", to.artist, to.name, from, to.id,
                                        if kept { "" } else { " (it couldn't be lined up: the fuller one kept)" }));
        }
    }

    /// One-off: songs kept twice (the same ISRC, or the same artist and title under two ids, learned before songs
    /// were kept under one id) are merged into one. Returns how many entries were merged away.
    pub fn merge_duplicates(&self) -> usize {
        let before = self.learned_tracks().len();
        let mut done: Vec<String> = Vec::new();
        for t in self.learned_tracks() {
            if done.contains(&t.id) || !self.song_path(&t.id, "json").exists() {
                continue;
            }
            done.push(self.canonical(&t).id);
        }
        before - self.learned_tracks().len()
    }

    /// Every learned song's track (id, name, artist, …) as saved with it.
    pub fn learned_tracks(&self) -> Vec<Track> {
        std::fs::read_dir(self.root.join("songs")).into_iter().flatten().flatten()
            .filter(|e| is_song_meta(&e.path()))
            .filter_map(|e| serde_json::from_slice::<Value>(&std::fs::read(e.path()).ok()?).ok())
            .filter_map(|m| serde_json::from_value::<Track>(m["track"].clone()).ok())
            .collect()
    }

    /// Delete a take (to the Trash, so it can be recovered).
    pub fn delete_take(&self, id: &str) -> bool {
        self.take_dir(id).is_some_and(|d| to_trash(&d))
    }

    /// Syllable bars for a learned song, if made: (start frame, end frame, MIDI note, syllable), song frames.
    /// The syllables' words aren't kept, only where each sits in the song's lyrics, so `lyrics` (fetched again
    /// from LRCLIB) gives them back; without them, or if the lyrics have changed since, the bars come back with
    /// no words. Songs learned before this kept the words themselves, and those are read as they are.
    ///
    /// LRCLIB doesn't always answer with the same lyrics (another entry for the song, or a search when a
    /// rate-limited lookup failed): then each line is found by its own check (kept since), and for songs saved
    /// before that, the lines' syllables are put back on the bars sung in each line's time (`lyrics` already
    /// carrying the times lined up with the singer, see `lyrics_on_kept_times`), where they number the same.
    pub fn syllables(&self, id: &str, lyrics: Option<&[(f64, String)]>) -> Option<Vec<(i64, i64, i32, String)>> {
        let v: Value = serde_json::from_slice(&std::fs::read(self.song_path(id, "syl.json")).ok()?).ok()?;
        let same = lyrics.is_some_and(|l| v["check"].as_str() == Some(lyrics_check(l).as_str()));
        // the line each kept line is in these lyrics, when it isn't the same lyrics
        let moved: Option<Vec<Option<usize>>> = match (same, lyrics, v["lines"].as_array()) {
            (false, Some(l), Some(kept)) => Some(match_lines(&kept.iter().map(|h| h.as_str().unwrap_or("").to_string()).collect::<Vec<_>>(), l)),
            _ => None,
        };
        let mut bars: Vec<(i64, i64, i32, String)> = v["bars"].as_array()?.iter().filter_map(|b| {
            let text = match &b[3] {
                Value::String(t) => t.clone(),
                Value::Array(span) => lyrics.filter(|_| same || moved.is_some()).and_then(|l| {
                    let (line, start, len) = (span.first()?.as_u64()? as usize, span.get(1)?.as_u64()? as usize, span.get(2)?.as_u64()? as usize);
                    let line = if same { line } else { (*moved.as_ref()?.get(line)?)? };
                    l.get(line)?.1.get(start..start + len).map(String::from)
                }).unwrap_or_default(),
                _ if b[4].as_bool() == Some(true) => "~".to_string(),
                _ => String::new(),
            };
            Some((b[0].as_i64()?, b[1].as_i64()?, b[2].as_i64()? as i32, text))
        }).filter(|b| b.1 > b.0).collect();
        // bars whose syllable wasn't found where it sits (another entry; or, for songs saved before syllable_spans
        // went by the bars' times, lost to a syllable sung over the next line's first): their line's syllables
        if let Some(l) = lyrics.filter(|_| bars.iter().any(|b| b.3.trim().is_empty())) {
            crate::syllables::relabel(&mut bars, l);
        }
        (bars.len() >= 10).then_some(bars)
    }

    /// Keep syllable bars with where each syllable sits in `lyrics` (line, byte start, byte length), not its words.
    pub fn save_syllables(&self, id: &str, bars: &[crate::syllables::SyllableBar], lyrics: &[(f64, String)]) {
        let _writing = writing();
        let spans = syllable_spans(bars, lyrics);
        let v = json!({"check": lyrics_check(lyrics), "lines": line_checks(lyrics), "bars": bars.iter().zip(&spans).map(|(b, s)| {
            let held = b.3.trim() == "~";
            json!([b.0, b.1, b.2, s.map(|(l, a, n)| json!([l, a, n])).unwrap_or(Value::Null), held])
        }).collect::<Vec<_>>()});
        if let Err(e) = write_atomic(&self.song_path(id, "syl.json"), v.to_string().as_bytes()) {
            if !cfg!(test) {
                crate::engine::log(&format!("{id}: its syllable bars couldn't be saved: {e}"));
            }
        }
    }

    /// When each lyric word was heard sung, from the word timer's whole-song alignment of the separated singer when
    /// the song was learned (songs/<id>.words, JSON): times and places only, never the words. `heard` is the aligner's
    /// letters per line of `lyrics` (word, letter, start s, end s), `believed` the lines that placed the lyrics (see
    /// syllables::check_placement). Kept so a play's lyrics can be checked against the singing without a rerun.
    pub fn save_words(&self, id: &str, lyrics: &[(f64, String)], heard: &crate::syllables::Heard, believed: &crate::syllables::Heard) {
        let mut words: Vec<Value> = Vec::new();
        for (line, h) in heard.iter().enumerate() {
            let Some(letters) = h else { continue };
            let kept = believed.get(line).is_some_and(|b| b.is_some());
            let mut by_word: Vec<(usize, f64, f64)> = Vec::new();
            for &(w, _, a, b) in letters {
                match by_word.iter_mut().find(|x| x.0 == w) {
                    Some(x) => { x.1 = x.1.min(a); x.2 = x.2.max(b); }
                    None => by_word.push((w, a, b)),
                }
            }
            by_word.sort_by_key(|x| x.0);
            let r = |x: f64| (x * 1000.0).round() / 1000.0;
            words.extend(by_word.into_iter().map(|(w, a, b)| json!([line, w, r(a), r(b), kept as u8])));
        }
        let v = json!({"v": 1, "check": lyrics_check(lyrics), "lines": lyrics.len(), "made": now_iso(),
                       "fields": ["line", "word", "start_s", "end_s", "believed"], "words": words});
        let _ = write_atomic(&self.song_path(id, "words"), v.to_string().as_bytes());
    }

    /// The heard words kept with a song (see `save_words`).
    pub fn words(&self, id: &str) -> Option<Value> {
        serde_json::from_slice(&std::fs::read(self.song_path(id, "words")).ok()?).ok()
    }

    /// Lyrics lined up with the singer, kept with the song once it has been learned: only their times (and a
    /// check of the words they were made for), not the words, which come from LRCLIB each time. Songs learned
    /// before this kept the words, and those are given back as they are.
    pub fn kept_lyrics(&self, id: &str) -> Option<KeptLyrics> {
        let m = self.song_meta(id)?;
        if let (Some(times), Some(check)) = (m["lyric_times"].as_array(), m["lyric_check"].as_str()) {
            let lines = m["lyric_lines"].as_array().map(|v| v.iter().map(|h| h.as_str().unwrap_or("").to_string()).collect());
            return Some(KeptLyrics::Times(Kept { times: times.iter().filter_map(|t| t.as_f64()).collect(), check: check.to_string(), lines }));
        }
        serde_json::from_value(m["lyrics"].clone()).ok().map(KeptLyrics::Text)
    }

    /// For the lab's test stores only (scored offline, with no lyrics lookup): keep the words too.
    pub fn save_lyrics_keeping_words(&self, id: &str, lyrics: &[(f64, String)]) {
        let _writing = writing();
        if let Some(mut m) = self.song_meta(id) {
            m["lyrics"] = json!(lyrics);
            if let Some(o) = m.as_object_mut() {
                o.remove("lyric_times");
                o.remove("lyric_check");
            }
            let _ = write_atomic(&self.song_path(id, "json"), m.to_string().as_bytes());
        }
    }

    pub fn save_lyrics(&self, id: &str, lyrics: &[(f64, String)]) {
        let _writing = writing();
        if let Some(mut m) = self.song_meta(id) {
            m["lyric_times"] = json!(lyrics.iter().map(|l| l.0).collect::<Vec<_>>());
            m["lyric_check"] = json!(lyrics_check(lyrics));
            m["lyric_lines"] = json!(line_checks(lyrics));
            if let Some(o) = m.as_object_mut() {
                o.remove("lyrics");
            }
            let _ = write_atomic(&self.song_path(id, "json"), m.to_string().as_bytes());
        }
    }

    /// The album cover kept for a learned song, and its Apple Music page if the cover is Apple's.
    pub fn cover(&self, id: &str) -> Option<crate::music::Cover> {
        let m = self.song_meta(id)?;
        Some(crate::music::Cover { art: m["track"]["art"].as_str()?.to_string(), link: m["track"]["link"].as_str().map(String::from) })
    }

    /// Find covers for learned songs that don't have one yet (songs learned before covers were kept), and the
    /// Apple Music page for Apple covers kept before their links were (an Apple cover is shown linked to it).
    pub fn find_missing_covers(&self) {
        for e in std::fs::read_dir(self.root.join("songs")).into_iter().flatten().flatten() {
            if !is_song_meta(&e.path()) {
                continue;
            }
            let read = || std::fs::read(e.path()).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
            let Some(m) = read() else { continue };
            let art = m["track"]["art"].as_str();
            if art.is_some_and(|a| !is_apple_art(a) || m["track"]["link"].is_string()) {
                continue;
            }
            let had_art = art.is_some();
            // looked up (over the network) outside the store's lock; then the song's file is read again under it,
            // as a take may have saved the song meanwhile, and left alone if its cover changed
            let found = crate::source::cover_for(&m["track"]);
            let _writing = writing();
            let Some(mut m) = read().filter(|now| now["track"]["art"] == m["track"]["art"]) else { continue };
            if let Some(c) = found {
                if !had_art {
                    m["track"]["art"] = json!(c.art);
                }
                if let Some(link) = c.link {
                    m["track"]["link"] = json!(link);
                } else if had_art {
                    continue;
                }
                let _ = write_atomic(&e.path(), m.to_string().as_bytes());
            }
        }
    }

    /// A learned song was just played (sung or learned): it goes first in the library.
    pub fn played(&self, id: &str) {
        let _writing = writing();
        if let Some(mut m) = self.song_meta(id) {
            m["played"] = json!(now_iso());
            let _ = write_atomic(&self.song_path(id, "json"), m.to_string().as_bytes());
        }
    }

    /// Learned songs, the last played or learned first, with your best score from your takes and how many there are.
    pub fn library(&self) -> Vec<Value> {
        let usual = self.usual_app();
        let mut best: std::collections::HashMap<String, i64> = Default::default();
        let mut last: std::collections::HashMap<String, String> = Default::default();
        let mut sung: std::collections::HashMap<String, u32> = Default::default();
        for t in self.takes() {
            let id = t["track"]["id"].as_str().unwrap_or("").to_string();
            if let Some(r) = t["recorded"].as_str() {
                let e = last.entry(id.clone()).or_default();
                if r > e.as_str() {
                    *e = r.to_string();
                }
            }
            if t["coverage"].as_f64().unwrap_or(0.0) >= 0.1 {
                *sung.entry(id.clone()).or_default() += 1;
                let s = t["score"].as_i64().unwrap_or(0);
                let e = best.entry(id).or_insert(s);
                *e = (*e).max(s);
            }
        }
        let mut rows: Vec<Value> = std::fs::read_dir(self.root.join("songs")).into_iter().flatten().flatten()
            .filter(|e| is_song_meta(&e.path()))
            .filter_map(|e| serde_json::from_slice::<Value>(&std::fs::read(e.path()).ok()?).ok())
            .filter(|m| m["track"]["id"].is_string())
            .map(|m| {
                let id = m["track"]["id"].as_str().unwrap_or("").to_string();
                let frac = m["fraction"].as_f64().unwrap_or(0.0);
                // the last thing that happened to it: learned, played, or a take of it
                let at = [m["learned"].as_str(), m["played"].as_str(), last.get(&id).map(|s| s.as_str())]
                    .into_iter().flatten().max().unwrap_or("").to_string();
                json!({"track": m["track"], "learned": m["learned"], "fraction": frac, "difficulty": self.difficulty(&id), "takes": sung.get(&id).copied().unwrap_or(0),
                       "ready": frac >= READY_COVERAGE, "best": best.get(&id), "active": at,
                       "favorite": m["favorite"].as_bool().unwrap_or(false), "favorited_at": m["favorited_at"],
                       "app": pick_app(&id, &aliases(&m), m["played_in"].as_str().and_then(app_of_key), usual, None).app.name()})
            }).collect();
        rows.sort_by(|a, b| b["active"].as_str().unwrap_or("").cmp(a["active"].as_str().unwrap_or("")));
        rows
    }

    /// How hard a learned song is to sing, 1 (easy) to 5 (very hard), from its notes (`difficulty`).
    pub fn difficulty(&self, id: &str) -> Option<u8> {
        let v: Value = serde_json::from_slice(&std::fs::read(self.song_path(id, "syl.json")).ok()?).ok()?;
        let bars: Vec<(i64, i64, i64)> = v["bars"].as_array()?.iter()
            .filter_map(|b| Some((b[0].as_i64()?, b[1].as_i64()?, b[2].as_i64()?))).collect();
        difficulty(&bars)
    }

    /// Every take's summary, newest first.
    pub fn takes(&self) -> Vec<Value> {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(self.takes_dir()).into_iter().flatten().flatten()
            .map(|e| e.path()).filter(|p| p.is_dir()).collect();
        dirs.sort();
        dirs.reverse();
        dirs.iter().filter_map(|d| take_summary(d)).map(|mut t| {
            // the learned song's cover (and Apple Music page) when the take has none, or the page for the same cover
            if !t["track"]["link"].is_string() {
                if let Some(c) = t["track"]["id"].as_str().and_then(|id| self.cover(id)) {
                    let art = t["track"]["art"].as_str();
                    if art.is_none() || art == Some(c.art.as_str()) {
                        t["track"]["art"] = json!(c.art);
                        if let Some(link) = c.link {
                            t["track"]["link"] = json!(link);
                        }
                    }
                }
            }
            t
        }).collect()
    }

    /// A take's folder, takes/<id>, if `id` is a take id (`valid_take_id`) and it's there. Every IPC command that
    /// names a take (take, take_audio, delete_take) comes through here.
    pub fn take_dir(&self, id: &str) -> Option<PathBuf> {
        let d = self.takes_dir().join(id);
        (valid_take_id(id) && d.is_dir()).then_some(d)
    }

    pub fn take_result(&self, id: &str) -> Option<Value> {
        let d = self.take_dir(id)?;
        let mut r: Value = serde_json::from_slice(&std::fs::read(d.join("result.json")).ok()?).ok()?;
        // where in the song the recording starts, for playing it back with the song
        if let Some(m) = std::fs::read(d.join("meta.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
            r["song_offset"] = m["song_offset"].clone();
        }
        // every take's recording is kept, up to a cap for them all (takeaudio::retain): past it the oldest go
        r["audio_kept"] = json!(crate::takeaudio::recording(&d).is_some());
        Some(r)
    }
}

/// The app's word in a song's .json ("played_in") and a take's result ("app").
pub fn app_key(app: Source) -> Option<&'static str> {
    match app {
        Source::Spotify => Some("spotify"),
        Source::Music => Some("music"),
        Source::YouTube => Some("youtube"),
        Source::Other => None,
    }
}

fn app_of_key(k: &str) -> Option<Source> {
    match k {
        "spotify" => Some(Source::Spotify),
        "music" => Some(Source::Music),
        "youtube" => Some(Source::YouTube),
        _ => None,
    }
}

/// The ids a song is known by in the apps (Spotify's, Apple Music's, YouTube's), the latest first.
fn aliases(meta: &Value) -> Vec<String> {
    meta["aliases"].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// Keep `alias` as an id of the song kept as `id`, if it's an app's own id (not an ISRC or Shazam's) and new.
fn add_alias(meta: &mut Value, id: &str, alias: &str) -> bool {
    let mut a = aliases(meta);
    if alias.is_empty() || alias == id || Source::of_id(alias) == Source::Other || a.iter().any(|x| x == alias) {
        return false;
    }
    a.insert(0, alias.to_string());
    meta["aliases"] = json!(a);
    true
}

/// The app with the most takes (each take's app, else its track id's); ties go to the latest take's.
fn usual_app(takes: &[Value]) -> Option<Source> {
    let mut n = [0usize; 3];
    let mut first: Option<Source> = None;
    for t in takes {
        let app = t["app"].as_str().and_then(app_of_key).unwrap_or_else(|| Source::of_id(t["track"]["id"].as_str().unwrap_or("")));
        if let Some(i) = Source::ALL.iter().position(|s| *s == app) {
            n[i] += 1;
            first.get_or_insert(app);
        }
    }
    let most = *n.iter().max()?;
    if most == 0 {
        return None;
    }
    first.filter(|f| n[Source::ALL.iter().position(|s| s == f).unwrap_or(0)] == most)
        .or_else(|| Source::ALL.iter().zip(n).find(|(_, k)| *k == most).map(|(s, _)| *s))
}

/// A song's syllable bars are too few when they span under half of where it's sung: its lyrics' span (first to last
/// line) when known, else its length (`duration` s; unknown: any bars do). A short play once replaced whole songs'
/// bars with its own (Shake It Off, 19 Oct: 21 s of a 4-minute song), and a learned song's plays are sung, not
/// learned, so only a play that makes bars when they're this thin repairs it. (By the lyrics' span, a song with a
/// long instrumental end isn't separated again on every play.)
pub fn thin_bars(bars: &[crate::syllables::SyllableBar], duration: f64, lyrics: Option<(f64, f64)>) -> bool {
    let (Some(a), Some(b)) = (bars.iter().map(|b| b.0).min(), bars.iter().map(|b| b.1).max()) else { return true };
    let sung = lyrics.map(|(f, l)| l - f).filter(|s| *s > 10.0).unwrap_or(duration);
    sung > 0.0 && ((b - a) as f64 * crate::dsp::FRAME_S) < 0.5 * sung
}

/// A play's track, keeping what the song already knew where the play knew less: a play from YouTube (a video's
/// title, thumbnail and length, no Apple Music page) or one named without a length mustn't replace a
/// Spotify-learned song's name, cover, page or length. `*_guessed`: the length is a guess (how far a play got,
/// when Apple's lookup failed, or a video's), not the app's or Apple's figure; `new_youtube`: the play was a
/// YouTube video. Returns the track and whether its length is a guess.
///  - length: an exact one wins (the play's over the song's); between two guesses the longer (the shorter is a
///    play cut short: The Kill, 19 Oct, paused at 3:17, became 3:17 long and "116% learned")
///  - name, artist, album: a video's don't replace a song's from an app (Shazam, Spotify, Music); otherwise the
///    play's, so a renamed video or the app's name for a song first learned from YouTube take over; a blank one
///    never replaces a known one
///  - cover and page: the song's Apple cover and page over a play's other cover
pub fn keep_known(old: &Track, old_guessed: bool, mut new: Track, new_guessed: bool, new_youtube: bool) -> (Track, bool) {
    let mut guessed = new_guessed;
    if old.duration > 0.0 && (new.duration <= 0.0 || (new_guessed && (!old_guessed || old.duration > new.duration))) {
        new.duration = old.duration;
        guessed = old_guessed;
    }
    let old_youtube = old.id.starts_with(crate::youtube::ID_PREFIX) || old.art.as_deref().is_some_and(|a| a.contains("ytimg.com"));
    let keep_names = new_youtube && !old_youtube;
    for (o, n) in [(&old.name, &mut new.name), (&old.artist, &mut new.artist), (&old.album, &mut new.album)] {
        if !o.trim().is_empty() && (keep_names || n.trim().is_empty()) {
            *n = o.clone();
        }
    }
    let apple = |t: &Track| t.art.as_deref().is_some_and(is_apple_art);
    if apple(old) && !apple(&new) {
        new.art = old.art.clone();
        new.link = old.link.clone().or(new.link);
    } else if new.link.is_none() {
        new.link = old.link.clone();
    }
    (new, guessed)
}

thread_local! {
    /// What the play being saved on this thread knew less well (see keep_known): (length guessed, a YouTube
    /// video). Each play is scored and saved on its own thread, so a note can't outlive its play or reach another.
    static PLAY_NOTE: std::cell::Cell<(bool, bool)> = const { std::cell::Cell::new((false, false)) };
}

/// The play this thread is about to save has a guessed length and/or is a YouTube video (see keep_known).
pub fn note_play(length_guessed: bool, youtube: bool) {
    PLAY_NOTE.with(|n| {
        let (g, y) = n.get();
        n.set((g || length_guessed, y || youtube));
    });
}

fn take_play_note() -> (bool, bool) {
    PLAY_NOTE.with(|n| n.replace((false, false)))
}

/// Where a song opens: the app, and its id there when known (a Spotify track id, an Apple Music or YouTube one).
#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    pub app: Source,
    pub id: Option<String>,
}

/// Which app to open a song in (`id`: the id it's kept under; `aliases`: its ids in the apps; `played_in`: the app
/// it was last played in; `usual`: the app you sing from most; `running`: the app being listened to now):
/// Spotify by its track id when it was last played there or Spotify is your usual app; else the app it was last
/// played in, your usual app, or the app running, in that order; else Spotify (a search). Never Apple Music
/// unless you've played something there.
pub fn pick_app(id: &str, aliases: &[String], played_in: Option<Source>, usual: Option<Source>, running: Option<Source>) -> Pick {
    let ids: Vec<&str> = std::iter::once(id).chain(aliases.iter().map(|a| a.as_str())).collect();
    let id_in = |app: Source| ids.iter().find(|i| Source::of_id(i) == app).map(|i| i.to_string());
    let real = |s: Option<Source>| s.filter(|s| *s != Source::Other);
    // a song kept by an app's own id was played there
    let played_in = real(played_in).or(real(Some(Source::of_id(id))));
    let spotify = id_in(Source::Spotify);
    if spotify.is_some() && (played_in == Some(Source::Spotify) || usual == Some(Source::Spotify)) {
        return Pick { app: Source::Spotify, id: spotify };
    }
    match played_in.or(real(usual)).or(real(running)) {
        Some(app) => Pick { app, id: id_in(app) },
        None => Pick { app: Source::Spotify, id: spotify },
    }
}

/// Set a song's heart in its .json, and when it was given (kept, not cleared, when it's taken away).
fn mark_favorite(meta: &mut Value, on: bool, at: &str) {
    meta["favorite"] = json!(on);
    if on {
        meta["favorited_at"] = json!(at);
    }
}

/// Move a file or folder to the user's Trash (~/.Trash), named so it doesn't clash; deleted outright if that fails.
fn to_trash(p: &Path) -> bool {
    if !p.exists() {
        return false;
    }
    if let Some(home) = std::env::var_os("HOME") {
        let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let dest = Path::new(&home).join(".Trash").join(format!("OnKey {} {name}", now_iso().replace(':', ".")));
        if std::fs::rename(p, &dest).is_ok() {
            return true;
        }
    }
    if p.is_dir() { std::fs::remove_dir_all(p).is_ok() } else { std::fs::remove_file(p).is_ok() }
}

/// songs/<track>.json (not the .syl.json syllable files beside it)
/// Two tracks with the same artist and title (letters and digits, any case): the same song under two ids.
pub fn same_title(a: &Track, b: &Track) -> bool {
    let norm = |s: &str| s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect::<String>();
    !norm(&a.name).is_empty() && norm(&a.name) == norm(&b.name) && norm(&a.artist) == norm(&b.artist)
}

fn is_song_meta(p: &Path) -> bool {
    p.extension().is_some_and(|x| x == "json") && !p.to_string_lossy().ends_with(".syl.json")
}

/// A cover image from Apple (iTunes Search or Shazam's artwork), which is shown linked to its Apple Music page.
fn is_apple_art(url: &str) -> bool {
    url.strip_prefix("https://").and_then(|r| r.split('/').next()).is_some_and(|host| host.ends_with(".mzstatic.com"))
}

fn take_summary(d: &Path) -> Option<Value> {
    let r: Value = serde_json::from_slice(&std::fs::read(d.join("result.json")).ok()?).ok()?;
    Some(json!({"id": d.file_name()?.to_string_lossy(), "track": r["track"], "recorded": r["recorded"],
        "score": r["score"], "accuracy": r["accuracy"], "coverage": r["coverage"], "within50": r["within50"],
        "bias_cents": r["bias_cents"], "latency_ms": r["latency_ms"], "live_score": r["live"]["score"],
        "repeat": r["live"]["cached"].as_bool().unwrap_or(false), "mode": r["mode"], "blind": r["blind"],
        "notes_hit": r["notes_hit"], "notes_total": r["notes_total"], "app": r["app"]}))
}

pub fn now_iso() -> String {
    chrono_like(std::time::SystemTime::now())
}

/// Local time as 2026-10-13T09:15:00 (via the C library, to avoid a date crate).
pub fn chrono_like(t: std::time::SystemTime) -> String {
    format_time(t, "%Y-%m-%dT%H:%M:%S")
}

pub fn format_time(t: std::time::SystemTime, fmt: &str) -> String {
    let secs = t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    unsafe {
        let mut tm: libc_tm = std::mem::zeroed();
        localtime_r(&secs, &mut tm);
        let mut buf = [0u8; 64];
        let f = std::ffi::CString::new(fmt).unwrap();
        let n = strftime(buf.as_mut_ptr() as *mut i8, buf.len(), f.as_ptr(), &tm);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

#[repr(C)]
struct libc_tm {
    tm_sec: i32, tm_min: i32, tm_hour: i32, tm_mday: i32, tm_mon: i32, tm_year: i32, tm_wday: i32, tm_yday: i32,
    tm_isdst: i32, tm_gmtoff: i64, tm_zone: *const i8,
}

extern "C" {
    fn localtime_r(t: *const i64, out: *mut libc_tm) -> *mut libc_tm;
    fn strftime(s: *mut i8, max: usize, fmt: *const i8, tm: *const libc_tm) -> usize;
}

pub fn fit<T: Clone>(x: &[T], n: usize, fill: T) -> Vec<T> {
    let mut out = vec![fill; n];
    let k = n.min(x.len());
    out[..k].clone_from_slice(&x[..k]);
    out
}

/// x moved earlier by sh frames (later if negative), keeping its length.
pub fn shift<T: Clone>(x: &[T], sh: i64, fill: T) -> Vec<T> {
    let n = x.len() as i64;
    (0..n).map(|i| {
        let j = i + sh;
        if j >= 0 && j < n { x[j as usize].clone() } else { fill.clone() }
    }).collect()
}

/// Frames by which a newly learned part sits later than the stored song, checked where the two overlap
/// (searching ±5 s). Some(0) when they don't overlap enough to check; None when they overlap but the
/// fingerprint can't confirm the placement.
fn fingerprint_shift(new_env: &[[f32; FP_BANDS]], old_env: &[[f32; FP_BANDS]], new_cov: &[bool], old_cov: &[bool]) -> Option<i64> {
    let n = new_env.len().min(new_cov.len());
    let old_cov = fit(old_cov, old_env.len(), false);
    let overlap = |i: usize| new_cov[i] && old_cov.get(i).copied().unwrap_or(false);
    // the longest stretch both cover; probe 10 s from inside it
    let (mut best, mut run_start, mut run) = ((0usize, 0usize), 0usize, 0usize);
    for i in 0..n {
        if overlap(i) {
            if run == 0 {
                run_start = i;
            }
            run += 1;
            if run > best.1 {
                best = (run_start, run);
            }
        } else {
            run = 0;
        }
    }
    if best.1 < 1200 {
        return Some(0);
    }
    let a = best.0 + 100;
    let probe = &new_env[a..a + 1000];
    let masked: Vec<[f32; FP_BANDS]> = old_env.iter().zip(&old_cov).map(|(r, c)| if *c { *r } else { [0.0; FP_BANDS] }).collect();
    let (k, conf) = locate(probe, &masked, a as i64 - 500, Some(a as i64 + 501));
    (conf > 1.3).then_some(a as i64 - k)
}

/// A learned song's lyrics as kept: their times only (with a check of the words), or, for songs learned before
/// that, the lyrics themselves.
pub enum KeptLyrics {
    Times(Kept),
    Text(Vec<(f64, String)>),
}

/// A learned song's lyric line times, with a check of the words they were made for (all of them, and each line's).
#[derive(Clone, Debug)]
pub struct Kept {
    pub times: Vec<f64>,
    pub check: String,
    /// each line's check (songs learned before these were kept have none)
    pub lines: Option<Vec<String>>,
}

/// Fetched lyrics (`fetched`, LRCLIB's times) on the times kept for a learned song. The same lyrics: the kept times.
/// Otherwise (LRCLIB answered with another entry), each line found among the kept ones by its own check takes that
/// line's time, or, for songs kept before lines had checks, each line the kept times clearly pair with (in order,
/// LRCLIB's timing moved as a whole, within 1 s). Lines not paired move with the nearest paired ones. None when
/// fewer than half the sung lines pair up: other lyrics than the song was learned with.
pub fn lyrics_on_kept_times(kept: &Kept, fetched: &[(f64, String)]) -> Option<Vec<(f64, String)>> {
    if kept.times.len() == fetched.len() && lyrics_check(fetched) == kept.check {
        return Some(fetched.iter().zip(&kept.times).map(|(l, t)| (*t, l.1.clone())).collect());
    }
    let pairs: Vec<Option<usize>> = match &kept.lines {
        Some(lines) => {
            let mut inv = vec![None; fetched.len()];
            for (k, j) in match_lines(lines, fetched).into_iter().enumerate() {
                if let Some(j) = j {
                    inv[j] = Some(k);
                }
            }
            inv
        }
        None => pair_by_time(&kept.times, fetched),
    };
    let sung = fetched.iter().filter(|l| !l.1.trim().is_empty()).count();
    let paired: Vec<(usize, f64)> = pairs.iter().enumerate().filter_map(|(i, k)| Some((i, kept.times.get((*k)?)? - fetched[i].0))).collect();
    if sung == 0 || paired.len() * 2 < sung {
        return None;
    }
    let mut out: Vec<(f64, String)> = fetched.iter().enumerate().map(|(i, (t, text))| {
        let t = match pairs[i].and_then(|k| kept.times.get(k)) {
            Some(k) => *k,
            None => {
                let mut near = paired.clone();
                near.sort_by_key(|(j, _)| j.abs_diff(i));
                let mut d: Vec<f32> = near.iter().take(3).map(|x| x.1 as f32).collect();
                t + crate::dsp::median(&mut d) as f64
            }
        };
        (t, text.clone())
    }).collect();
    for i in 1..out.len() {
        if out[i].0 < out[i - 1].0 {
            out[i].0 = out[i - 1].0;
        }
    }
    Some(out)
}

/// How a learned song's play got its lyrics (see learned_play).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LearnedHow {
    /// the entry the kept times were made for: exactly the kept times
    Exact,
    /// another entry, its lines paired with the kept times, and the result fits the singer's phrases (kept with
    /// the song from now on)
    Repaired,
    /// another entry whose pairing doesn't fit the singing as well: shown paired, but not kept
    Unchecked,
    /// no entry pairs with the kept times: LRCLIB's own timing
    Unfit,
}

/// A learned song's lyrics for one play.
#[derive(Debug, Clone)]
pub struct LearnedPlay {
    /// the lines to show (song s)
    pub lyrics: Vec<(f64, String)>,
    /// the LRCLIB entry they're from (LRCLIB's own times): what's pinned for the song's next plays
    pub entry: Vec<(f64, String)>,
    pub how: LearnedHow,
}

/// Share of the sung lines that start within 0.6 s of a phrase start (`starts`, s, sorted) of the learned singing.
fn on_phrases(lyrics: &[(f64, String)], starts: &[f64]) -> f64 {
    let sung: Vec<f64> = lyrics.iter().filter(|l| l.1.chars().any(|c| c.is_alphanumeric())).map(|l| l.0).collect();
    let hit = sung.iter().filter(|&&t| {
        let i = starts.partition_point(|&p| p < t - 0.6);
        starts.get(i).is_some_and(|p| (p - t).abs() <= 0.6)
    }).count();
    hit as f64 / sung.len().max(1) as f64
}

/// A learned song's lyrics for a play, the same every play. `candidates` are LRCLIB entries for the song: the one
/// pinned on this Mac first (lyrics::learned), then LRCLIB's answers. The entry the kept times were made for, if any
/// candidate is it, gets exactly the kept times. Otherwise the first entry whose lines pair with the kept times
/// (lyrics_on_kept_times) is shown paired: kept with the song (Repaired: its times saved and the entry pinned, so
/// later plays are Exact) only when it fits the learned singing (`voiced`, song frames of the learned line) at least
/// nearly as well as the kept times do (and half its lines on phrase starts); a pairing that doesn't is shown but not kept (Unchecked). None pair: the
/// first entry with LRCLIB's timing (Unfit). None when there are no candidates.
pub fn learned_play(kept: &Kept, candidates: &[Vec<(f64, String)>], voiced: Option<&[bool]>) -> Option<LearnedPlay> {
    if let Some(e) = candidates.iter().find(|e| kept.times.len() == e.len() && lyrics_check(e) == kept.check) {
        let lyrics = e.iter().zip(&kept.times).map(|(l, t)| (*t, l.1.clone())).collect();
        return Some(LearnedPlay { lyrics, entry: e.clone(), how: LearnedHow::Exact });
    }
    for e in candidates {
        if let Some(lyrics) = lyrics_on_kept_times(kept, e) {
            let fits = voiced.is_some_and(|v| {
                let starts = crate::lyrics_align::phrase_starts(v);
                // the kept times as the reference (which of them are sung isn't known without their words)
                let mut k: Vec<(f64, String)> = kept.times.iter().map(|t| (*t, "x".to_string())).collect();
                k.dedup_by(|a, b| (a.0 - b.0).abs() < 0.05);
                let got = on_phrases(&lyrics, &starts);
                got >= 0.5 && got >= on_phrases(&k, &starts) - 0.1
            });
            return Some(LearnedPlay { lyrics, entry: e.clone(), how: if fits { LearnedHow::Repaired } else { LearnedHow::Unchecked } });
        }
    }
    candidates.first().map(|e| LearnedPlay { lyrics: e.clone(), entry: e.clone(), how: LearnedHow::Unfit })
}

/// Each line's own check (FNV-1a of its words, 8 hex digits).
pub fn line_checks(lyrics: &[(f64, String)]) -> Vec<String> {
    lyrics.iter().map(|(_, l)| {
        let mut h: u32 = 0x811c9dc5;
        for b in l.trim().bytes() {
            h ^= b as u32;
            h = h.wrapping_mul(0x01000193);
        }
        format!("{h:08x}")
    }).collect()
}

/// For each kept line (by its check), the line of `lyrics` with the same words, in order (longest common
/// sequence); empty lines aren't paired.
fn match_lines(kept: &[String], lyrics: &[(f64, String)]) -> Vec<Option<usize>> {
    let now = line_checks(lyrics);
    let empty = line_checks(&[(0.0, String::new())])[0].clone();
    let (n, m) = (kept.len(), now.len());
    let mut d = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            d[i][j] = if kept[i] == now[j] && kept[i] != empty { d[i + 1][j + 1] + 1 } else { d[i + 1][j].max(d[i][j + 1]) };
        }
    }
    let mut out = vec![None; n];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if kept[i] == now[j] && kept[i] != empty {
            out[i] = Some(j);
            i += 1;
            j += 1;
        } else if d[i + 1][j] >= d[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// Songs kept before lines had checks: pair fetched lines with kept times by time alone. LRCLIB's entries for one
/// song differ by a near-constant shift (another release, another transcriber) or slightly in pace: the shift
/// (and a pace within 2%) that pairs the most sung lines within 1 s, each line then paired with the nearest kept
/// time if within 1 s of it, in order. Returns, per fetched line, the kept line it pairs with.
fn pair_by_time(kept: &[f64], fetched: &[(f64, String)]) -> Vec<Option<usize>> {
    let sung: Vec<usize> = (0..fetched.len()).filter(|&i| !fetched[i].1.trim().is_empty()).collect();
    let nearest = |t: f64| -> Option<(usize, f64)> {
        let i = kept.partition_point(|&k| k < t);
        [i.checked_sub(1), Some(i)].into_iter().flatten().filter(|&k| k < kept.len())
            .map(|k| (k, (kept[k] - t).abs())).min_by(|a, b| a.1.total_cmp(&b.1))
    };
    let t0 = sung.first().map_or(0.0, |&i| fetched[i].0);
    let mut best = (0usize, 0.0f64, 0.0f64, 1.0f64);       // (paired, error, shift, pace)
    for pace in [0.98, 0.985, 0.99, 0.995, 1.0, 1.005, 1.01, 1.015, 1.02] {
        let mut shift = -10.0;
        while shift <= 10.0 + 1e-9 {
            let (mut n, mut err) = (0usize, 0.0f64);
            for &i in &sung {
                if let Some((_, e)) = nearest(t0 + (fetched[i].0 - t0) * pace + shift).filter(|x| x.1 <= 1.0) {
                    n += 1;
                    err += e;
                }
            }
            if n > best.0 || (n == best.0 && err < best.1) {
                best = (n, err, shift, pace);
            }
            shift += 0.05;
        }
    }
    let (_, _, shift, pace) = best;
    let mut out = vec![None; fetched.len()];
    let mut last: Option<usize> = None;
    for &i in &sung {
        if let Some((k, _)) = nearest(t0 + (fetched[i].0 - t0) * pace + shift).filter(|x| x.1 <= 1.0) {
            if last.map_or(true, |l| k > l) {
                out[i] = Some(k);
                last = Some(k);
            }
        }
    }
    out
}

/// A short check of the lyrics' words (FNV-1a of the lines), so lyrics fetched again can be matched with the
/// times kept for them.
pub fn lyrics_check(lyrics: &[(f64, String)]) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for (i, (_, line)) in lyrics.iter().enumerate() {
        for b in line.bytes().chain((i + 1 < lyrics.len()).then_some(b'\n')) {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    format!("{h:016x}")
}

/// Where each bar's syllable sits in the lyrics the bars were made from (lined up with the singer): (line, byte
/// start, byte length), or None for a held note ("~") or a syllable that can't be found (its words then aren't
/// shown). Found in order, from where the last one was, in the lines sung around the bar's time (from 2 s before a
/// line to 2 s after the next one starts): found by order alone (in that line or the next three), a line's last
/// syllable sung over the next line's first (bars go by time, so it comes after it) was found lines further on, and
/// lines with no bars (a part not heard) left the rest behind, and every syllable after that went unfound. A
/// syllable sung over the next line's first is found in the line before (or earlier in this one), and the place
/// it's found from stays.
pub fn syllable_spans(bars: &[crate::syllables::SyllableBar], lyrics: &[(f64, String)]) -> Vec<Option<(usize, usize, usize)>> {
    let (mut line, mut pos) = (0usize, 0usize);
    bars.iter().map(|b| {
        let t = b.3.trim();
        if t.is_empty() || t == "~" {
            return None;
        }
        let at = b.0 as f64 * crate::dsp::FRAME_S;
        for (k, l) in lyrics.iter().enumerate().skip(line) {
            if k > line && l.0 > at + 2.0 {
                break;
            }
            if lyrics.get(k + 1).is_some_and(|n| n.0 + 2.0 < at) {
                continue;
            }
            let from = if k == line { pos } else { 0 };
            if let Some(a) = find_ignoring_case(&l.1, t, from) {
                (line, pos) = (k, a + t.len());
                return Some((k, a, t.len()));
            }
        }
        [line.checked_sub(1), Some(line)].into_iter().flatten()
            .find_map(|k| find_ignoring_case(&lyrics.get(k)?.1, t, 0).map(|a| (k, a, t.len())))
    }).collect()
}

fn find_ignoring_case(hay: &str, needle: &str, from: usize) -> Option<usize> {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    (from..=h.len().checked_sub(n.len())?)
        .find(|&i| hay.is_char_boundary(i) && hay.is_char_boundary(i + n.len()) && h[i..i + n.len()].eq_ignore_ascii_case(n))
}

#[cfg(test)]
mod tests {
    #[test]
    fn bars_spanning_under_half_the_song_are_too_few() {
        let bar = |a: i64, b: i64| (a, b, 60, "la".to_string());
        assert!(thin_bars(&[], 240.0, None));
        // Shake It Off: 3:11 to 3:32 of 4:02
        assert!(thin_bars(&[bar(19_100, 19_150), bar(21_150, 21_200)], 241.6, None));
        assert!(!thin_bars(&[bar(300, 350), bar(24_650, 24_700)], 263.4, None));
        // length unknown: any bars do
        assert!(!thin_bars(&[bar(19_100, 19_150)], 0.0, None));
        // sung only in its first 100 s of 300 (a long instrumental end): bars over 90 of them are plenty
        assert!(!thin_bars(&[bar(1_000, 1_050), bar(9_950, 10_000)], 300.0, Some((5.0, 100.0))));
        assert!(thin_bars(&[bar(1_000, 1_050), bar(9_950, 10_000)], 300.0, None));
    }

    #[test]
    fn a_play_that_knew_less_keeps_what_the_song_knew() {
        let t = |d: f64, art: Option<&str>, link: Option<&str>, name: &str| Track { id: "isrc:X".into(), name: name.into(), artist: "A".into(),
            album: String::new(), duration: d, art: art.map(String::from), link: link.map(String::from) };
        let apple = Some("https://is1-ssl.mzstatic.com/image/thumb/x/600x600bb.jpg");
        let thumb = Some("https://i.ytimg.com/vi/x/hqdefault.jpg");
        let spotify_learned = t(231.0, apple, Some("https://music.apple.com/gb/album/x/1?i=2"), "The Kill (Bury Me)");
        // a YouTube play: a video's title, thumbnail and length (a guess), no page: the song keeps its own
        let (k, g) = keep_known(&spotify_learned, false, t(236.0, thumb, None, "The Kill (Official Video)"), true, true);
        assert_eq!((k.name.as_str(), k.art.as_deref(), k.link.is_some(), k.duration, g), ("The Kill (Bury Me)", apple, true, 231.0, false));
        // a song first learned from YouTube: the app's name (or a renamed video's) takes over
        let video = t(236.0, thumb, None, "The Kill (Official Video)");
        let (k, g) = keep_known(&video, true, t(231.0, apple, None, "The Kill (Bury Me)"), false, false);
        assert_eq!((k.name.as_str(), k.duration, g), ("The Kill (Bury Me)", 231.0, false));
        assert_eq!(keep_known(&video, true, t(236.0, thumb, None, "The Kill"), true, true).0.name, "The Kill");
        // a blank name never replaces a known one
        assert_eq!(keep_known(&spotify_learned, false, t(231.0, None, None, " "), false, false).0.name, "The Kill (Bury Me)");
        // a play named without a length, guessed from where it paused: the song's exact length stays
        let (k, g) = keep_known(&spotify_learned, false, t(197.2, None, None, "The Kill (Bury Me)"), true, false);
        assert_eq!((k.duration, g), (231.0, false));
        // two guesses: the longer; an exact length beats a guess, shorter or not
        let guessed = t(229.7, None, None, "The Kill (Bury Me)");
        assert_eq!(keep_known(&guessed, true, t(197.2, None, None, "x"), true, false).0.duration, 229.7);
        assert_eq!(keep_known(&guessed, true, t(240.0, None, None, "x"), true, false).0.duration, 240.0);
        assert_eq!(keep_known(&guessed, true, t(228.0, None, None, "x"), false, false).0.duration, 228.0);
        // a play with no length at all keeps the song's
        assert_eq!(keep_known(&guessed, true, t(0.0, None, None, "x"), false, false).0.duration, 229.7);
    }

    #[test]
    fn a_play_note_goes_to_its_own_save_only() {
        note_play(true, false);
        note_play(false, true);
        assert_eq!(take_play_note(), (true, true));
        // taken once: the next save on this thread has none
        assert_eq!(take_play_note(), (false, false));
        // another thread's note isn't this one's
        std::thread::spawn(|| note_play(true, true)).join().unwrap();
        assert_eq!(take_play_note(), (false, false));
    }

    use super::*;

    #[test]
    fn syllables_found_in_the_lyrics_and_given_back() {
        // made-up words (no song lyrics in the repo)
        let lyrics = vec![(10.0, "We've walked the hills, sung them all,".to_string()), (14.0, String::new()),
                          (15.5, "Lanterns, glowing, by the Old Mill now".to_string())];
        let syl = ["We've", "walked", "the", "hills,", "sung", "~", "them", "all,", "Lan", "~", "terns,", "glow", "ing,", "by", "the", "Old", "Mill", "now", "xyz"];
        // bars 0.3 s apart from 10 s, the second line's from 15.5 s
        let bars: Vec<crate::syllables::SyllableBar> = syl.iter().enumerate()
            .map(|(i, s)| { let f = if i < 8 { 1000 + 30 * i as i64 } else { 1550 + 30 * (i - 8) as i64 }; (f, f + 25, 60, s.to_string()) }).collect();
        let back = |spans: Vec<Option<(usize, usize, usize)>>, lyrics: &[(f64, String)]| -> Vec<String> {
            spans.iter().map(|s| s.map_or(String::new(), |(l, a, n)| lyrics[l].1[a..a + n].to_string())).collect()
        };
        let want: Vec<String> = syl.iter().map(|s| if *s == "~" || *s == "xyz" { String::new() } else { s.to_string() }).collect();
        assert_eq!(back(syllable_spans(&bars, &lyrics), &lyrics), want);
        // a line's last syllable sung over the next line's first (so it comes after it): it, and every syllable
        // after it, still found (it was found two lines on, and the rest of the song lost)
        let lyrics = vec![(10.0, "Paper lanterns all aglow".to_string()), (12.0, "Down the harbour, row by row".to_string()),
                          (14.0, "Lanterns for the long way home".to_string()), (20.0, "Sing it low, aglow, aglow".to_string())];
        let syl = ["Pa", "per", "lan", "terns", "all", "a", "Down", "glow", "the", "har", "bour,", "row", "by", "row", "Lan", "terns", "for", "the", "long", "way", "home"];
        let bars: Vec<crate::syllables::SyllableBar> = syl.iter().enumerate().map(|(i, s)| {
            let f = [1000, 1030, 1060, 1090, 1120, 1150, 1200, 1210, 1240, 1270, 1300, 1330, 1360, 1390][..].get(i).copied().unwrap_or(1400 + 30 * (i as i64 - 14));
            (f, f + 25, 60, s.to_string())
        }).collect();
        assert_eq!(back(syllable_spans(&bars, &lyrics), &lyrics), syl.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(syllable_spans(&bars, &lyrics)[7], Some((0, 20, 4)));
        assert_eq!(lyrics_check(&lyrics), lyrics_check(&lyrics.iter().map(|l| (0.0, l.1.clone())).collect::<Vec<_>>()));
        assert_ne!(lyrics_check(&lyrics), lyrics_check(&lyrics[..2]));
    }

    #[test]
    fn kept_times_go_on_other_lyrics_for_the_song() {
        // made-up words; kept times lined up with the singer, ~1 s before LRCLIB's first entry
        let learned: Vec<(f64, String)> = [(10.0, "Lanterns by the mill"), (14.0, "Walking down the hill"), (18.5, ""),
            (21.0, "Lanterns by the mill"), (25.0, "Singing down the hill")].iter().map(|(t, s)| (*t, s.to_string())).collect();
        let times = vec![9.1, 13.2, 17.4, 20.0, 24.3];
        let with_lines = Kept { times: times.clone(), check: lyrics_check(&learned), lines: Some(line_checks(&learned)) };
        // the same lyrics: the kept times
        assert_eq!(lyrics_on_kept_times(&with_lines, &learned).unwrap().iter().map(|l| l.0).collect::<Vec<_>>(), times);
        // another entry: other times (+0.1 s), one line spelt differently, an extra line at the end
        let mut other = learned.iter().map(|(t, s)| (t + 0.1, s.clone())).collect::<Vec<_>>();
        other[1].1 = "Walkin' down the hill".into();
        other.push((29.0, "Lanterns by the mill".into()));
        let got = lyrics_on_kept_times(&with_lines, &other).unwrap();
        assert_eq!(got[0].0, 9.1);
        assert_eq!(got[3].0, 20.0);
        assert!((got[1].0 - 13.2).abs() < 0.3, "{got:?}");    // unpaired: moved with its neighbours
        assert!((got[5].0 - 28.1).abs() < 0.3, "{got:?}");
        // kept before lines had checks: paired by time (LRCLIB's 0.9-1.0 s late, 0.5% slower)
        let old = Kept { times: times.clone(), check: "x".into(), lines: None };
        let slower = learned.iter().map(|(t, s)| (t * 1.005 + 0.9, s.clone())).collect::<Vec<_>>();
        let got = lyrics_on_kept_times(&old, &slower).unwrap();
        for (g, t) in got.iter().zip(&times) {
            if !g.1.is_empty() {
                assert_eq!(g.0, *t);
            }
        }
        // other words altogether: none
        let other_song = vec![(3.0, "a".to_string()), (40.0, "b".to_string()), (80.0, "c".to_string())];
        assert!(lyrics_on_kept_times(&old, &other_song).is_none());
    }

    #[test]
    fn a_learned_song_shows_the_same_lyrics_whatever_lrclib_answers() {
        // made-up words; kept times made for `learned`, the singer starting each phrase at those times
        let learned: Vec<(f64, String)> = [(10.0, "Lanterns by the mill"), (14.0, "Walking down the hill"), (18.5, ""),
            (21.0, "Lanterns by the mill"), (25.0, "Singing down the hill")].iter().map(|(t, s)| (*t, s.to_string())).collect();
        let times = vec![9.1, 13.2, 17.4, 20.0, 24.3];
        let kept = Kept { times: times.clone(), check: lyrics_check(&learned), lines: None };
        let mut other = learned.iter().map(|(t, s)| (t + 0.1, s.clone())).collect::<Vec<_>>();
        other[1].1 = "Walkin' down the hill".into();
        let unrelated = vec![(3.0, "a".to_string()), (40.0, "b".to_string()), (80.0, "c".to_string())];
        // the entry it was learned with, wherever it is among the answers (pinned or not): exactly the kept times
        for cands in [vec![learned.clone()], vec![other.clone(), learned.clone()], vec![unrelated.clone(), other.clone(), learned.clone()]] {
            let p = learned_play(&kept, &cands, None).unwrap();
            assert_eq!(p.how, LearnedHow::Exact);
            assert_eq!(p.lyrics, times.iter().zip(&learned).map(|(t, l)| (*t, l.1.clone())).collect::<Vec<_>>());
            assert_eq!(p.entry, learned);
        }
        // another entry: paired; kept with the song only when it fits the singing (voiced from each kept time for 2 s)
        let mut voiced = vec![false; 3000];
        for t in &times {
            let k = (t * 100.0) as usize;
            voiced[k..k + 200].iter_mut().for_each(|v| *v = true);
        }
        let p = learned_play(&kept, &[other.clone()], Some(&voiced)).unwrap();
        assert_eq!(p.how, LearnedHow::Repaired);
        for (l, t) in p.lyrics.iter().zip(&times).filter(|(l, _)| !l.1.is_empty()) {
            assert_eq!(l.0, *t);
        }
        assert_eq!(learned_play(&kept, &[other.clone()], Some(&vec![false; 3000])).unwrap().how, LearnedHow::Unchecked);
        assert_eq!(learned_play(&kept, &[other], None).unwrap().how, LearnedHow::Unchecked);
        // nothing pairs: LRCLIB's timing; nothing at all: none
        assert_eq!(learned_play(&kept, &[unrelated.clone()], None).unwrap().lyrics, unrelated);
        assert!(learned_play(&kept, &[], None).is_none());
    }

    #[test]
    fn a_song_learned_under_another_id_is_found_by_its_title() {
        // learned from Shazam's match (by ISRC), then played with Spotify's own id: the same song, ready to sing
        let dir = std::env::temp_dir().join(format!("onkey-knownas-{}", std::process::id()));
        let store = Store::new(dir.clone());
        std::fs::create_dir_all(dir.join("songs")).unwrap();
        let t = |id: &str, name: &str| Track { id: id.into(), name: name.into(), artist: "Ed Sheeran".into(), duration: 100.0, ..Default::default() };
        let learned = t("isrc:GBAHS1700004", "Castle on the Hill");
        let n = (100.0 / crate::dsp::FRAME_S) as usize;
        assert!(store.save_song(&learned, vec![60.0; n], vec![true; n], vec![[1.0; FP_BANDS]; n]));
        let spotify = t("spotify:track:6PCUP3dWmTjcTtXY02oFdT", "Castle On The Hill");
        let found = store.known_as(&spotify).expect("found by title");
        assert_eq!(found.id, learned.id);
        assert!(store.load_song(&found.id).is_some_and(|s| s.ready(found.duration)));
        assert!(store.known_as(&t("spotify:track:x", "Castle on the Hill - Acoustic")).is_none(), "another version");
        assert!(store.known_as(&learned).is_none(), "kept under its own id");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_song_is_kept_once_whatever_id_it_was_played_by() {
        let dir = std::env::temp_dir().join(format!("onkey-dupes-{}", std::process::id()));
        let store = Store::new(dir.clone());
        std::fs::create_dir_all(dir.join("songs")).unwrap();
        let t = |id: &str, name: &str| Track { id: id.into(), name: name.into(), artist: "Adele".into(), duration: 100.0, ..Default::default() };
        let n = (100.0 / crate::dsp::FRAME_S) as usize;
        let onset: Vec<[f32; FP_BANDS]> = (0..n).map(|i| [((i * 37 % 101) as f32) / 10.0; FP_BANDS]).collect();
        let part = |a: usize, b: usize| (0..n).map(|i| (a..b).contains(&i)).collect::<Vec<bool>>();
        // learned twice before: a ready entry by ISRC and 10% under Spotify's id; and a song only Spotify ever named, twice
        assert!(store.save_song(&t("isrc:USSM12105970", "Easy On Me"), vec![60.0; n], part(0, 9000), onset.clone()));
        assert!(store.save_song(&t("spotify:track:46IZ0fSY2mpAiktS3KOqds", "Easy on Me"), vec![60.0; n], part(9000, 10000), onset.clone()));
        assert!(store.save_song(&t("spotify:track:a", "Hello"), vec![60.0; n], part(0, 5000), onset.clone()));
        assert!(store.save_song(&t("spotify:track:b", "Hello"), vec![60.0; n], part(5000, 10000), onset.clone()));
        assert_eq!(store.merge_duplicates(), 2);
        let ids: Vec<String> = store.learned_tracks().into_iter().map(|t| t.id).collect();
        assert_eq!(ids.len(), 2, "{ids:?}");
        assert!(ids.contains(&"isrc:USSM12105970".to_string()), "{ids:?}");
        // merged by fingerprint: the ISRC entry now has both parts
        assert!(store.load_song("isrc:USSM12105970").unwrap().fraction(100.0) > 0.99);
        assert!(store.load_song("spotify:track:46IZ0fSY2mpAiktS3KOqds").is_none());
        // a later play by Spotify's id goes to the ISRC entry, and one by the ISRC takes over an entry by Spotify's id
        assert_eq!(store.canonical(&t("spotify:track:46IZ0fSY2mpAiktS3KOqds", "Easy On Me")).id, "isrc:USSM12105970");
        let hello = ids.iter().find(|i| i.starts_with("spotify:")).unwrap().clone();
        assert_eq!(store.canonical(&t("isrc:GBX", "Hello")).id, "isrc:GBX");
        assert!(store.load_song(&hello).is_none() && store.load_song("isrc:GBX").is_some());
        assert_eq!(store.merge_duplicates(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_app_a_song_opens_in() {
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let pick = |app, id: Option<&str>| Pick { app, id: id.map(String::from) };
        let (sp, mu, yt) = (Some(Source::Spotify), Some(Source::Music), Some(Source::YouTube));
        // How To Save A Life: learned from Spotify, kept by its ISRC, nothing playing now: Spotify, by its track id
        let a = ids(&["spotify:track:5fVZ"]);
        assert_eq!(pick_app("isrc:X", &a, sp, None, None), pick(Source::Spotify, Some("spotify:track:5fVZ")));
        assert_eq!(pick_app("isrc:X", &a, None, sp, None), pick(Source::Spotify, Some("spotify:track:5fVZ")));
        // last played in Apple Music (your usual app isn't Spotify): there, by its id
        let both = ids(&["music:AB12", "spotify:track:5fVZ"]);
        assert_eq!(pick_app("isrc:X", &both, mu, mu, sp), pick(Source::Music, Some("music:AB12")));
        // played on YouTube last, though Spotify is usual: Spotify's id wins (you usually sing from Spotify)
        assert_eq!(pick_app("isrc:X", &ids(&["youtube:v1", "spotify:track:5fVZ"]), yt, sp, None), pick(Source::Spotify, Some("spotify:track:5fVZ")));
        assert_eq!(pick_app("isrc:X", &ids(&["youtube:v1"]), yt, sp, None), pick(Source::YouTube, Some("youtube:v1")));
        // only heard (an ISRC, no app ids): the app it played in, else your usual one, else the one running
        assert_eq!(pick_app("isrc:X", &[], sp, mu, None), pick(Source::Spotify, None));
        assert_eq!(pick_app("isrc:X", &[], None, sp, mu), pick(Source::Spotify, None));
        assert_eq!(pick_app("isrc:X", &[], None, None, mu), pick(Source::Music, None));
        // nothing known: never Apple Music; a song kept by an app's own id opens there
        assert_eq!(pick_app("isrc:X", &[], None, None, None), pick(Source::Spotify, None));
        assert_eq!(pick_app("music:AB12", &[], None, sp, None), pick(Source::Music, Some("music:AB12")));
        assert_eq!(pick_app("spotify:track:5fVZ", &[], None, None, mu), pick(Source::Spotify, Some("spotify:track:5fVZ")));
        // your usual app: the most takes (each take's app, else its id's)
        let take = |id: &str, app: Option<&str>| json!({"track": {"id": id}, "app": app});
        assert_eq!(usual_app(&[take("isrc:A", Some("spotify")), take("music:B", None), take("spotify:track:C", None)]), sp);
        assert_eq!(usual_app(&[take("isrc:A", None), take("shazam:1", None)]), None);
    }

    #[test]
    fn where_a_song_played_is_kept_with_it() {
        let dir = std::env::temp_dir().join(format!("onkey-played-in-{}", std::process::id()));
        let store = Store::new(dir.clone());
        let t = |id: &str, name: &str| Track { id: id.into(), name: name.into(), artist: "The Fray".into(), duration: 100.0, ..Default::default() };
        let n = (100.0 / crate::dsp::FRAME_S) as usize;
        let onset: Vec<[f32; FP_BANDS]> = (0..n).map(|i| [((i * 37 % 101) as f32) / 10.0; FP_BANDS]).collect();
        let part = |a: usize, b: usize| (0..n).map(|i| (a..b).contains(&i)).collect::<Vec<bool>>();
        let meta = |id: &str| store.song_meta(id).unwrap();

        // learned from Spotify, kept by its ISRC: Spotify's id and the app are kept, and survive another save
        let save = t("isrc:USSM10603618", "How to Save a Life");
        assert!(store.save_song(&save, vec![60.0; n], part(0, 9000), onset.clone()));
        store.note_play(&save.id, "spotify:track:5fVZ", Source::Spotify);
        assert!(store.save_song(&save, vec![60.0; n], part(9000, 10000), onset.clone()));
        assert_eq!(aliases(&meta(&save.id)), vec!["spotify:track:5fVZ"]);
        assert_eq!(meta(&save.id)["played_in"], "spotify");
        store.note_play(&save.id, &save.id, Source::Other);           // only heard: nothing new
        assert_eq!(aliases(&meta(&save.id)).len(), 1);
        let (p, track) = store.open_pick(&save.id, Some(Source::Music));
        assert_eq!(p, Pick { app: Source::Spotify, id: Some("spotify:track:5fVZ".into()) });
        assert_eq!(track.unwrap().name, "How to Save a Life");
        // found by its Spotify id too
        assert_eq!(store.open_pick("spotify:track:5fVZ", None).0.id.as_deref(), Some("spotify:track:5fVZ"));

        // the duplicate merge keeps the union of the ids (the merged-away entry's own id among them)
        assert!(store.save_song(&t("music:AB12", "Over My Head"), vec![60.0; n], part(0, 5000), onset.clone()));
        store.note_play("music:AB12", "music:AB12", Source::Music);
        assert!(store.save_song(&t("isrc:OVER", "Over My Head"), vec![60.0; n], part(5000, 10000), onset.clone()));
        store.note_alias("isrc:OVER", "youtube:v1");
        assert_eq!(store.merge_duplicates(), 1);
        let mut a = aliases(&meta("isrc:OVER"));
        a.sort();
        assert_eq!(a, vec!["music:AB12", "youtube:v1"]);
        assert_eq!(meta("isrc:OVER")["played_in"], "music");

        // backfilled at start-up: a song learned before this, merged away under Spotify's id, and History's takes
        assert!(store.save_song(&t("isrc:OLD", "You Found Me"), vec![60.0; n], part(0, 9000), onset.clone()));
        let merged = dir.join("songs").join("merged");
        std::fs::create_dir_all(&merged).unwrap();
        std::fs::write(merged.join("spotify_track_old.json"), json!({"track": t("spotify:track:old", "You Found Me")}).to_string()).unwrap();
        let take = dir.join("takes").join("20261001-120000-The-Fray-You-Found-Me");
        std::fs::create_dir_all(&take).unwrap();
        std::fs::write(take.join("result.json"), json!({"track": t("spotify:track:old", "You Found Me"), "recorded": "2026-10-01T12:00:00"}).to_string()).unwrap();
        assert_eq!(store.backfill_sources(), 1);
        assert_eq!(aliases(&meta("isrc:OLD")), vec!["spotify:track:old"]);
        assert_eq!(meta("isrc:OLD")["played_in"], "spotify");
        assert_eq!(store.backfill_sources(), 0, "once");
        assert_eq!(store.usual_app(), Some(Source::Spotify));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_heart_is_kept_with_the_song() {
        let dir = std::env::temp_dir().join(format!("onkey-fav-{}", std::process::id()));
        let store = Store::new(dir.clone());
        let t = |id: &str, name: &str| Track { id: id.into(), name: name.into(), artist: "Adele".into(), duration: 100.0, ..Default::default() };
        let n = (100.0 / crate::dsp::FRAME_S) as usize;
        let onset: Vec<[f32; FP_BANDS]> = (0..n).map(|i| [((i * 37 % 101) as f32) / 10.0; FP_BANDS]).collect();
        let part = |a: usize, b: usize| (0..n).map(|i| (a..b).contains(&i)).collect::<Vec<bool>>();
        let fav = |id: &str| store.library().into_iter().find(|r| r["track"]["id"] == id).map(|r| r["favorite"] == true);

        // set and get on a learned song; it stays through another save of the song
        let hello = t("isrc:GBHELLO", "Hello");
        assert!(store.save_song(&hello, vec![60.0; n], part(0, 9000), onset.clone()));
        assert!(!store.is_favorite(&hello) && fav("isrc:GBHELLO") == Some(false));
        store.set_favorite(&hello, true);
        assert!(store.is_favorite(&hello) && fav("isrc:GBHELLO") == Some(true));
        assert!(store.library()[0]["favorited_at"].is_string());
        assert!(store.save_song(&hello, vec![60.0; n], part(9000, 10000), onset.clone()));
        assert_eq!(fav("isrc:GBHELLO"), Some(true));
        // hearted by Spotify's id for the same song: it's the ISRC entry's heart
        assert!(store.is_favorite(&t("spotify:track:h", "Hello")));
        store.set_favorite(&t("spotify:track:h", "Hello"), false);
        assert_eq!(fav("isrc:GBHELLO"), Some(false));

        // hearted while being learned for the first time: it's the song's once saved
        let new = t("spotify:track:new", "Skyfall");
        store.set_favorite(&new, true);
        assert!(store.is_favorite(&new) && fav("spotify:track:new").is_none());
        assert!(store.save_song(&new, vec![60.0; n], part(0, 5000), onset.clone()));
        assert_eq!(fav("spotify:track:new"), Some(true));
        assert!(!dir.join("favorites-pending.json").exists());
        // and taken away again before it's saved: no heart
        let other = t("spotify:track:other", "Rolling in the Deep");
        store.set_favorite(&other, true);
        store.set_favorite(&other, false);
        assert!(store.save_song(&other, vec![60.0; n], part(0, 5000), onset.clone()));
        assert_eq!(fav("spotify:track:other"), Some(false));

        // the duplicate merge keeps it when either entry had it
        assert!(store.save_song(&t("isrc:GBSKY", "Skyfall"), vec![60.0; n], part(5000, 10000), onset.clone()));
        assert!(store.save_song(&t("isrc:GBROLL", "Rolling In The Deep"), vec![60.0; n], part(5000, 10000), onset.clone()));
        store.set_favorite(&t("isrc:GBROLL", "Rolling In The Deep"), true);
        assert_eq!(store.merge_duplicates(), 2);
        assert_eq!(fav("isrc:GBSKY"), Some(true), "the merged-away entry was the favourite");
        assert_eq!(fav("isrc:GBROLL"), Some(true), "the kept entry was the favourite");
        assert_eq!(fav("isrc:GBHELLO"), Some(false));

        // a heart given under Spotify's id to a song being learned, which is then kept by its ISRC
        let first = t("spotify:track:first", "Someone Like You");
        store.set_favorite(&first, true);
        assert!(store.save_song(&t("isrc:GBSLY", "Someone Like You"), vec![60.0; n], part(0, 9000), onset.clone()));
        assert_eq!(store.canonical(&first).id, "isrc:GBSLY");
        assert_eq!(fav("isrc:GBSLY"), Some(true));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn two_plays_of_one_song_saved_at_once_are_one_entry() {
        // a take split in two (Spotify's id, then the ISRC), both scored at the same moment
        let dir = std::env::temp_dir().join(format!("onkey-saveone-{}", std::process::id()));
        let store = Store::new(dir.clone());
        std::fs::create_dir_all(dir.join("songs")).unwrap();
        let t = |id: &str| Track { id: id.into(), name: "My Love, My Life".into(), artist: "ABBA".into(), duration: 100.0, ..Default::default() };
        let n = (100.0 / crate::dsp::FRAME_S) as usize;
        let onset: Vec<[f32; FP_BANDS]> = (0..n).map(|i| [((i * 37 % 101) as f32) / 10.0; FP_BANDS]).collect();
        let part = |a: usize, b: usize| (0..n).map(|i| (a..b).contains(&i)).collect::<Vec<bool>>();
        let threads: Vec<_> = [("spotify:track:7qbQ", 0, 4000), ("isrc:SEAYD7601030", 4000, 8000)].into_iter().map(|(id, a, b)| {
            let (store, track, onset, cov) = (store.clone(), t(id), onset.clone(), part(a, b));
            std::thread::spawn(move || store.save_as_one(&track, vec![60.0; n], cov, onset))
        }).collect();
        for th in threads {
            assert!(th.join().unwrap().0);
        }
        let ids: Vec<String> = store.learned_tracks().into_iter().map(|t| t.id).collect();
        assert_eq!(ids, vec!["isrc:SEAYD7601030".to_string()]);
        assert!(store.load_song("isrc:SEAYD7601030").unwrap().fraction(100.0) > 0.79);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn apple_covers_are_told_apart() {
        assert!(is_apple_art("https://is1-ssl.mzstatic.com/image/thumb/a/600x600bb.jpg"));
        assert!(!is_apple_art("https://i.scdn.co/image/ab67616d"));
        assert!(!is_apple_art("https://i.ytimg.com/vi/x/hqdefault.jpg"));
        assert!(!is_apple_art("https://evil.example/x.mzstatic.com/y"));
    }

    #[test]
    fn two_takes_saved_the_same_second_get_their_own_folders() {
        let dir = std::env::temp_dir().join(format!("onkey-takedirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let store = Store::new(dir.clone());
        let name = "20261015-120000-Adele-Hello";
        let (a, na) = store.new_take_dir(name).unwrap();
        let (b, nb) = store.new_take_dir(name).unwrap();
        assert_eq!(na, name);
        assert_eq!(nb, format!("{name}-2"));
        assert_ne!(a, b);
        assert!(a.is_dir() && b.is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn atomic_writes_leave_no_temp_files_and_old_ones_are_swept() {
        let dir = std::env::temp_dir().join(format!("onkey-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("songs")).unwrap();
        let p = dir.join("songs").join("x.json");
        write_atomic(&p, b"{\"a\":1}").unwrap();
        write_atomic(&p, b"{\"a\":2}").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"{\"a\":2}");
        let names = |d: &Path| std::fs::read_dir(d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect::<Vec<_>>();
        assert_eq!(names(&dir.join("songs")), vec!["x.json"], "no temp file left behind");
        // a temp file a killed OnKey left (another process's, old) is cleared when the store opens; a fresh one isn't
        let stale = dir.join("songs").join(".x.json.tmp-1-0");
        let fresh = dir.join("songs").join(".x.json.tmp-2-0");
        std::fs::write(&stale, b"{\"a\"").unwrap();
        std::fs::write(&fresh, b"{\"a\"").unwrap();
        let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        std::fs::File::options().write(true).open(&stale).unwrap().set_modified(hour_ago).unwrap();
        Store::new(dir.clone());
        assert!(!stale.exists() && fresh.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// How hard a melody is to sing, 1 (easy) to 5 (very hard), from its bars (start and end in 10 ms frames, MIDI note).
/// The range it covers sets the level: the span between its 10th and 90th percentile notes (so a stray note or an
/// octave slip doesn't count), 6 semitones or less 1, up to 9 2, up to 12 3, up to 15 4, more 5. A fast melody (over
/// 4.2 notes a second of singing) or a leapy one (over 15% of notes a jump of 5 semitones or more straight after the
/// last) is one harder. Bands set on 155 learned songs (18 Oct 2026) to spread them over the five levels; scores
/// didn't track any measure of the melody well enough to set them (range -0.13), so it's a guide, not a promise.
/// None under 40 bars: too little to judge.
pub fn difficulty(bars: &[(i64, i64, i64)]) -> Option<u8> {
    if bars.len() < 40 {
        return None;
    }
    let mut notes: Vec<i64> = bars.iter().map(|b| b.2).collect();
    notes.sort_unstable();
    let n = notes.len();
    let range = notes[n * 9 / 10] - notes[n / 10];
    let level = match range {
        ..=6 => 1,
        7..=9 => 2,
        10..=12 => 3,
        13..=15 => 4,
        _ => 5,
    };
    let singing: f64 = bars.iter().map(|b| (b.1 - b.0) as f64 / 100.0).sum();
    let fast = n as f64 / singing.max(1.0) > 4.2;
    let leaps = bars.windows(2).filter(|w| w[1].0 - w[0].1 < 30 && (w[1].2 - w[0].2).abs() >= 5).count();
    let leapy = leaps as f64 / n as f64 > 0.15;
    Some((level + u8::from(fast || leapy)).min(5))
}

#[cfg(test)]
mod difficulty_tests {
    use super::difficulty;

    /// `notes` sung one after another, each `len` frames with a 5-frame gap.
    fn bars(notes: &[i64], len: i64) -> Vec<(i64, i64, i64)> {
        notes.iter().enumerate().map(|(i, &m)| (i as i64 * (len + 5), i as i64 * (len + 5) + len, m)).collect()
    }

    #[test]
    fn levels_follow_range_speed_and_leaps() {
        let narrow: Vec<i64> = (0..60).map(|i| 60 + (i % 5)).collect();           // 4 semitones
        let wide: Vec<i64> = (0..60).map(|i| 55 + (i % 24)).collect();            // 23 semitones (18 from 10% to 90%)
        assert_eq!(difficulty(&bars(&narrow, 40)), Some(1));
        assert_eq!(difficulty(&bars(&wide, 40)), Some(5));
        assert_eq!(difficulty(&bars(&narrow, 15)), Some(2), "fast: over 4.2 notes a second");
        let leapy: Vec<i64> = (0..60).map(|i| if i % 2 == 0 { 60 } else { 65 }).collect();
        assert_eq!(difficulty(&bars(&leapy, 40)), Some(1 + 1), "a 5-semitone range, every note a leap");
        let mut stray = narrow.clone();
        stray[7] = 90;                                                           // one stray note doesn't count
        assert_eq!(difficulty(&bars(&stray, 40)), Some(1));
        assert_eq!(difficulty(&bars(&narrow[..30], 40)), None);
    }
}
