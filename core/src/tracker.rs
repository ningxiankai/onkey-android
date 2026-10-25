//! Where in the song the captured music is: a short list of candidate places, each with a weight built up by the
//! music's own evidence (after Arzt & Widmer's "any-time" music tracker, 2010).
//!
//! - Each fingerprint check gives the best few places the last stretch of music matches (dsp::locate_peaks),
//!   runners-up included: a repetitive intro matches a beat off nearly as well as in the right place, and both stay
//!   candidates until the music tells them apart.
//! - The music app's position and listening's answers are a broad guess: they nudge the candidates near them, never
//!   decide on their own (the app's lead varies with the output, and its jumps land seconds off).
//! - A jump (a song starting, a pause, a seek, a skip) opens the door to new places.
//! - What's shown moves to another candidate only when that one clearly leads and has done so over several seconds
//!   of music; small refinements of the same place are taken as they come.
//!
//! Places are song offsets: song time = offset + the take's seconds (a take's sound runs on in order, so a place,
//! once right, stays right until the music jumps).

/// A candidate place.
#[derive(Clone, Debug)]
pub struct Cand {
    /// song time minus take seconds (s)
    pub offset: f64,
    /// evidence for it: grows with each check that finds it, fades with each that doesn't
    pub weight: f64,
    /// how many checks found it, and when (take s) it was first and last found
    pub hits: u32,
    pub first: f64,
    pub last: f64,
    /// the best correlation it has been found with (a right place on clean music: ~0.9-1.0)
    pub peak: f32,
}

/// One decision, for the records: why the shown place is what it is.
#[derive(Clone, Debug)]
pub struct Decision {
    pub t: f64,
    pub from: Option<f64>,
    pub to: f64,
    pub why: &'static str,
    pub weight: f64,
    pub runner_up: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Tracker {
    cands: Vec<Cand>,
    shown: Option<f64>,
    /// a broad guess (offset, its uncertainty in s) from the music app or listening
    guess: Option<(f64, f64)>,
    /// until this take time, new places are welcome (just after a jump)
    open_until: f64,
    /// when the last start or jump was (take s)
    opened_at: f64,
    /// the music has matched the shown place strongly (since it was shown)
    shown_strong: bool,
    /// just after OnKey's own skip (it asked for the place, and the music app lands there a moment late): one sharp,
    /// decisive match close to the guess confirms it (see `skipped`)
    expect_near: bool,
    /// the song's beat period (s), when known: a place a whole number of beats from the one shown is an alias (a
    /// repetitive passage matches a beat or two off as well as in the right place) and needs more evidence
    beat: Option<f64>,
    /// places (offsets) the song itself repeats at, from the one shown (the music around here matches itself that far
    /// on): set by the caller before each check (Take::track, from the learned fingerprint)
    aliases: Vec<f64>,
    pub decisions: Vec<Decision>,
}

/// Places closer than this are the same place (wide enough to follow a version playing up to ~1% fast or slow
/// between checks; a beat-off match, ~0.4 s, stays apart).
const SAME_S: f64 = 0.08;
/// A peak counts as evidence only if it's at least this close to the best one's correlation.
const RUNNER_UP: f32 = 0.6;
/// Unconfirmed weight kept per check.
const FADE: f64 = 0.7;
/// To move what's shown to another place: it must outweigh the shown one this many times, and have been found
/// over this much music (Arzt: a candidate is accepted after it has tracked ~5 s).
const LEAD: f64 = 2.0;
const SUSTAIN_S: f64 = 4.0;
/// The least evidence (about one clear check) that moves what's shown away from the guess after a jump.
const MOVE_MIN: f64 = 0.8;
/// After a jump, how long new places are welcome.
const OPEN_S: f64 = 8.0;
/// A first match after a start or jump is taken at once when it's at least this good (normalised correlation)
/// and the next best place scores under this share of it.
const DISTINCT_MIN: f32 = 0.5;
const DISTINCT_RATIO: f32 = 0.6;
/// The music needed after a start or jump before one distinctive match may move the guess.
const SNAP_AFTER_S: f64 = 4.5;
/// How long a likely alias (a beat or two from the shown place) must lead before it's shown.
const ALIAS_SUSTAIN_S: f64 = 12.0;
/// A match this good is strong evidence; this good, decisive.
const STRONG: f32 = 0.6;
const DECISIVE: f32 = 0.8;
/// Closer than this to the shown place is the same music drifting, not another place.
const BESIDE_S: f64 = 0.15;
/// After OnKey's skip, a decisive match this close to the guess confirms the landing at once.
const NEAR_GUESS_S: f64 = 0.15;
/// How many candidates are kept.
const KEEP: usize = 8;

impl Tracker {
    pub fn new() -> Tracker {
        Tracker::default()
    }

    /// The song's beat period (s), from its learned fingerprint.
    pub fn set_beat(&mut self, period: f64) {
        if period > 0.2 && period < 2.0 {
            self.beat = Some(period);
        }
    }

    /// Offsets at which the song resembles itself around the shown place (dsp::self_similarity), for this check.
    pub fn set_aliases(&mut self, offsets: Vec<f64>) {
        self.aliases = offsets;
    }

    pub fn has_beat(&self) -> bool {
        self.beat.is_some()
    }

    /// Whether `offset` is a beat or two (or half a beat) from what's shown: a likely alias of it.
    fn alias(&self, offset: f64) -> bool {
        // (only a place the music has strongly confirmed is protected: a weakly matched wrong place a beat off must not
        // shield itself from the right one)
        if !self.shown_strong {
            return false;
        }
        if self.aliases.iter().any(|a| (a - offset).abs() < SAME_S) {
            return true;
        }
        let (Some(p), Some(s)) = (self.beat, self.shown) else { return false };
        let d = (offset - s).abs();
        [0.5, 1.0, 2.0].iter().any(|k| (d - k * p).abs() < 0.06)
    }

    /// What's shown now (None until there's a guess or evidence).
    pub fn offset(&self) -> Option<f64> {
        self.shown
    }

    /// Whether the music itself has confirmed the shown place.
    pub fn confirmed(&self) -> bool {
        self.shown.is_some_and(|s| self.cands.iter().any(|c| (c.offset - s).abs() < SAME_S && c.hits >= 2 && c.peak >= STRONG))
    }

    /// A broad guess at the place (the music app's position, listening's answer), uncertain by `sigma` s. With
    /// nothing shown yet, it's shown at once.
    pub fn guess(&mut self, offset: f64, sigma: f64, t: f64) {
        self.guess = Some((offset, sigma.max(0.05)));
        if self.shown.is_none() {
            self.open_until = t + OPEN_S;
            self.opened_at = t;
            self.show(offset, t, "first guess (music app or listening)", 0.0, 0.0);
        }
    }

    /// The music jumped (a song started, a pause ended, a seek or skip): new places are welcome for a while, and
    /// the evidence for the old place no longer counts for more than a new one's.
    pub fn jumped(&mut self, t: f64, guess: Option<(f64, f64)>) {
        self.expect_near = false;
        self.open_until = t + OPEN_S;
        self.opened_at = t;
        self.shown_strong = false;
        for c in self.cands.iter_mut() {
            c.weight = c.weight.min(0.5);
            c.hits = c.hits.min(1);
        }
        if let Some((o, s)) = guess {
            self.guess = Some((o, s.max(0.05)));
            self.show(o, t, "jump: the guess for where it landed", 0.0, 0.0);
        }
    }

    /// OnKey's own skip: a jump to a place it asked for, landing a moment late (allowed for in `guess`). The first
    /// check that matches decisively and sharply close to the guess confirms the landing (with the usual rules a
    /// second match was waited for, 0.5 s later, the notes dimmed meanwhile).
    pub fn skipped(&mut self, t: f64, guess: (f64, f64)) {
        self.jumped(t, Some(guess));
        self.expect_near = true;
    }

    /// One fingerprint check at take time `t`: the best few places (offset, correlation) the last stretch of
    /// music matched, best first.
    pub fn observe(&mut self, peaks: &[(f64, f32)], t: f64) {
        let Some(&(best_at, best)) = peaks.first() else { return };
        if best <= 0.0 {
            return;
        }
        // a distinctive match (well ahead of any other place) right after a start or jump is taken at once:
        // leaving the notes unsynced while a second check comes round is worse than the rare wrong first match,
        // which the following checks put right
        let second = peaks.get(1).map_or(0.0, |p| p.1);
        // (only to correct a guess that's clearly off: a match near the guess may be the guess refined or a weak
        // early match, and waits for the usual confirmation; live, a quiet intro's first 3 s matched 0.22 s off)
        let far_from_guess = self.shown.map_or(true, |s| (best_at - s).abs() >= 0.3);
        // (and only from enough music: in 3 s a repetitive intro matched a beat off, 0.37 s, as distinctly as a right
        // place; a longer stretch tells beats apart)
        let enough = t - self.opened_at >= SNAP_AFTER_S;
        let distinct = best >= DISTINCT_MIN && second < DISTINCT_RATIO * best && far_from_guess && enough && !self.alias(best_at);
        let mut found = vec![false; self.cands.len()];
        // how much this check is worth: a sharp match (well ahead of the next place) of decent quality counts fully;
        // an ambiguous or weak one hardly at all (live, mediocre matches found twice walked a right place 9 s off)
        let sharp = (best / second.max(0.05)) as f64;
        // (a right place on clean music matches at ~0.9-1.0; the end of the song before still in the sound, live
        // under autoplay, matched a wrong place at 0.33-0.44 for 5 s)
        let quality = ((sharp - 1.0) / 0.5).clamp(0.0, 1.0) * (((best - 0.15) / 0.25) as f64).clamp(0.0, 1.0);
        for &(offset, r) in peaks.iter().filter(|(_, r)| *r >= RUNNER_UP * best) {
            // the best match counts most; a runner-up less, steeply (a near tie is a real doubt, a far one isn't)
            let mut e = quality * ((r / best) as f64).powi(4);
            // the guess (the music app, listening) is a prior: places near it count more, places far from it much
            // less (a chorus elsewhere in the song matches too)
            if let Some((g, s)) = self.guess {
                e *= 0.25 + 1.25 * (-0.5 * ((offset - g) / s).powi(2)).exp();
            }
            match self.cands.iter().position(|c| (c.offset - offset).abs() < SAME_S) {
                Some(i) => {
                    let c = &mut self.cands[i];
                    // (the same place, refined: a small move toward the newest)
                    c.offset += (offset - c.offset) * 0.5;
                    c.weight = c.weight * 0.9 + e;
                    c.hits += 1;
                    c.last = t;
                    c.peak = c.peak.max(r);
                    found[i] = true;
                }
                None => {
                    self.cands.push(Cand { offset, weight: e, hits: 1, first: t, last: t, peak: r });
                    found.push(true);
                }
            }
        }
        for (c, f) in self.cands.iter_mut().zip(&found) {
            if !f {
                c.weight *= FADE;
            }
        }
        self.cands.retain(|c| c.weight > 0.02);
        self.cands.sort_by(|a, b| b.weight.total_cmp(&a.weight));
        self.cands.truncate(KEEP);
        // OnKey's skip landed about where it asked: a sharp, decisive match beside the guess is the landing (not
        // a beat alias: those are a beat or half a beat away, and a near tie isn't sharp)
        if std::mem::take(&mut self.expect_near) {
            let near = self.guess.is_some_and(|(g, _)| (best_at - g).abs() < NEAR_GUESS_S);
            if near && best >= DECISIVE && second < DISTINCT_RATIO * best && !self.alias(best_at) {
                if let Some(c) = self.cands.iter_mut().find(|c| (c.offset - best_at).abs() < SAME_S) {
                    c.hits = c.hits.max(2);
                    let w = c.weight;
                    self.show(best_at, t, "OnKey's skip: the music confirms it landed beside the guess", w, second as f64);
                    self.shown_strong = true;
                    return;
                }
            }
        }
        if distinct && t <= self.open_until && self.shown.map_or(true, |s| (s - best_at).abs() >= SAME_S)
            && self.cands.iter().find(|c| (c.offset - best_at).abs() < SAME_S).is_some_and(|c| c.hits == 1)
            && !self.cands.iter().any(|c| c.hits >= 2 && (c.offset - best_at).abs() >= SAME_S && self.shown.is_some_and(|s| (c.offset - s).abs() < SAME_S)) {
            let w = self.cands.iter().find(|c| (c.offset - best_at).abs() < SAME_S).map_or(0.0, |c| c.weight);
            self.show(best_at, t, "a distinctive match right after a start or jump", w, second as f64);
            return;
        }
        self.decide(t);
    }

    fn decide(&mut self, t: f64) {
        let Some(top) = self.cands.first().cloned() else { return };
        let runner_up = self.cands.get(1).map_or(0.0, |c| c.weight);
        let Some(shown) = self.shown else {
            if top.hits >= 2 {
                self.show(top.offset, t, "found by the music", top.weight, runner_up);
            }
            return;
        };
        if self.cands.iter().any(|c| (c.offset - shown).abs() < SAME_S && c.peak >= STRONG && c.last == t) {
            self.shown_strong = true;
        }
        // decisive: a place matched strongly, where the shown one has only ever matched weakly (live, the end of the
        // song before matched 0.4 for seconds; the right place then matched 0.98)
        let shown_peak = self.cands.iter().find(|c| (c.offset - shown).abs() < SAME_S).map_or(0.0, |c| c.peak);
        let decisive = self.cands.iter()
            .filter(|c| c.hits >= 2 && c.peak >= DECISIVE && c.last == t && (c.offset - shown).abs() >= SAME_S)
            .max_by(|a, b| a.peak.total_cmp(&b.peak)).cloned();
        if let Some(d) = decisive.filter(|d| shown_peak < STRONG && !self.shown_strong && !self.alias(d.offset)) {
            self.show(d.offset, t, "a strong match where the shown place only matched weakly", d.weight, top.weight);
            return;
        }
        if (top.offset - shown).abs() < SAME_S {
            // the shown place, refined
            if (top.offset - shown).abs() > 0.002 {
                self.shown = Some(top.offset);
            }
            return;
        }
        let shown_w = self.cands.iter().find(|c| (c.offset - shown).abs() < SAME_S).map_or(0.0, |c| c.weight);
        let sustained = top.hits >= 3 && top.last - top.first >= SUSTAIN_S;
        let open = t <= self.open_until;
        // just after a jump, a place the music finds twice, where the shown one has no support, is taken at once
        // (with real evidence: live, a weak place found twice in a quiet intro, weight 0.16, moved a right guess 1.9 s)
        // (a place a beat or two from the shown one is a likely alias: only long, sustained evidence moves to it)
        let alias = self.alias(top.offset);
        // (and a weakly matched place never takes over by persistence alone: a repetitive intro or the song before)
        // (a place just beside the shown one, closer than any beat, is the same music drifting: a version playing a
        // little fast or slow matches weakly as the stretch smears it)
        let beside = (top.offset - shown).abs() < BESIDE_S;
        let sustained = sustained && (top.peak >= STRONG || beside) && (!alias || top.last - top.first >= ALIAS_SUSTAIN_S);
        // decisive: a place matched strongly, where the shown one has only ever matched weakly (live, the end of the
        // song before matched 0.4 for seconds; the right place then matched 0.98)
         if open && !alias && t - self.opened_at >= SNAP_AFTER_S && top.hits >= 2 && top.peak >= STRONG && top.weight >= MOVE_MIN && shown_w < 0.3 * top.weight {
            self.show(top.offset, t, "after a jump: the music found where it landed", top.weight, runner_up);
        } else if sustained && top.weight >= LEAD * shown_w.max(0.1) {
            self.show(top.offset, t, "the music has favoured another place for several seconds", top.weight, runner_up);
        }
    }

    fn show(&mut self, offset: f64, t: f64, why: &'static str, weight: f64, runner_up: f64) {
        #[cfg(feature = "devtest")]
        if crate::devtest::knob!("TRACKER_TRACE").is_some() {
            eprintln!("    tracker t {t:6.1}: {:?} -> {offset:+.3} ({why}; w {weight:.2}, runner-up {runner_up:.2}; candidates {:?})", self.shown,
                      self.cands.iter().map(|c| ((c.offset * 100.0).round() / 100.0, (c.weight * 100.0).round() / 100.0, c.hits)).collect::<Vec<_>>());
        }
        self.decisions.push(Decision { t, from: self.shown, to: offset, why, weight, runner_up });
        if self.decisions.len() > 200 {
            self.decisions.remove(0);
        }
        self.shown = Some(offset);
        self.shown_strong = false;
    }

    /// The current guess (offset), the centre of the search.
    pub fn guess_offset(&self) -> Option<f64> {
        self.guess.map(|g| g.0)
    }

    pub fn candidates(&self) -> &[Cand] {
        &self.cands
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wrong_guess_is_corrected_by_the_music() {
        // the app's guess is 1.7 s off; the music finds the right place every second
        let mut tr = Tracker::new();
        tr.guess(1.7, 0.5, 0.0);
        for k in 0..12 {
            tr.observe(&[(0.0, 0.8), (0.44, 0.7)], 3.0 + k as f64);
        }
        assert!((tr.offset().unwrap() - 0.0).abs() < 0.01, "{:?}", tr.offset());
    }

    #[test]
    fn a_beat_off_runner_up_never_takes_over() {
        // shown at the right place; a beat-off match nearly as good keeps turning up
        let mut tr = Tracker::new();
        tr.guess(0.0, 0.3, 0.0);
        for k in 0..30 {
            let (a, b) = if k % 3 == 0 { ((0.44, 0.81), (0.0, 0.8)) } else { ((0.0, 0.8), (0.44, 0.78)) };
            tr.observe(&[a, b], 3.0 + k as f64);
        }
        assert!(tr.offset().unwrap().abs() < 0.01, "{:?}", tr.offset());
    }

    #[test]
    fn after_a_jump_the_landing_place_is_found() {
        let mut tr = Tracker::new();
        tr.guess(0.0, 0.3, 0.0);
        for k in 0..10 {
            tr.observe(&[(0.0, 0.8)], 3.0 + k as f64);
        }
        // a skip asked for +30 s; the app landed 3.5 s further
        tr.jumped(13.0, Some((30.0, 1.0)));
        for k in 0..6 {
            tr.observe(&[(33.5, 0.75)], 16.0 + k as f64);
        }
        assert!((tr.offset().unwrap() - 33.5).abs() < 0.01, "{:?}", tr.offset());
    }

    #[test]
    fn a_distinctive_first_match_snaps_at_once() {
        let mut tr = Tracker::new();
        tr.guess(1.7, 0.5, 0.0);
        tr.observe(&[(0.0, 0.8), (6.0, 0.4)], 5.0);
        assert!(tr.offset().unwrap().abs() < 0.01, "{:?}", tr.offset());
    }

    #[test]
    fn an_ambiguous_first_match_waits() {
        let mut tr = Tracker::new();
        tr.guess(1.7, 0.5, 0.0);
        tr.observe(&[(0.0, 0.8), (0.44, 0.75)], 5.0);
        assert!((tr.offset().unwrap() - 1.7).abs() < 0.01, "{:?}", tr.offset());
    }

    #[test]
    fn a_near_guess_early_match_waits() {
        let mut tr = Tracker::new();
        tr.guess(4.327, 0.5, 0.0);
        tr.observe(&[(4.55, 0.6), (8.0, 0.2)], 3.0);
        assert!((tr.offset().unwrap() - 4.327).abs() < 0.01, "{:?}", tr.offset());
    }

    #[test]
    fn weak_evidence_never_moves_a_guess() {
        let mut tr = Tracker::new();
        tr.guess(-0.514, 0.5, 0.0);
        tr.observe(&[(1.35, 0.3), (2.0, 0.27)], 3.0);
        tr.observe(&[(1.35, 0.3), (2.0, 0.27)], 3.5);
        assert!((tr.offset().unwrap() + 0.514).abs() < 0.01, "{:?}", tr.offset());
    }

    #[test]
    fn an_early_beat_off_match_does_not_snap() {
        let mut tr = Tracker::new();
        tr.guess(0.0, 0.5, 0.0);
        tr.observe(&[(0.37, 0.45), (5.0, 0.24)], 3.0);
        assert!(tr.offset().unwrap().abs() < 0.01, "{:?}", tr.offset());
    }

    #[test]
    fn a_beat_alias_needs_long_evidence() {
        // the right place confirmed, then a repetitive passage: for 10 s only the place one beat (0.37 s) off matches
        let mut tr = Tracker::new();
        tr.set_beat(0.37);
        tr.guess(0.0, 0.5, 0.0);
        tr.observe(&[(0.0, 0.9), (5.0, 0.2)], 3.0);
        tr.observe(&[(0.0, 0.9), (5.0, 0.2)], 3.5);
        for k in 0..20 {
            tr.observe(&[(0.37, 0.65), (5.0, 0.2)], 4.0 + 0.5 * k as f64);
        }
        assert!(tr.offset().unwrap().abs() < 0.01, "{:?}", tr.offset());
    }

    #[test]
    fn a_weak_wrong_place_gives_way_to_a_strong_one_a_beat_off() {
        // autoplay: the song before still in the sound matched a beat off, weakly, for a while; then the right place
        let mut tr = Tracker::new();
        tr.set_beat(0.47);
        tr.guess(0.4, 0.5, 0.0);
        for k in 0..12 {
            tr.observe(&[(0.4, 0.4), (2.8, 0.2)], 3.0 + 0.5 * k as f64);
        }
        tr.observe(&[(-0.07, 0.98), (0.4, 0.35)], 9.5);
        tr.observe(&[(-0.07, 0.98), (0.4, 0.35)], 10.0);
        assert!((tr.offset().unwrap() + 0.07).abs() < 0.01, "{:?}", tr.offset());
    }

    #[test]
    fn onkeys_skip_is_confirmed_by_one_decisive_match_beside_the_guess() {
        let mut tr = Tracker::new();
        tr.guess(0.0, 0.3, 0.0);
        for k in 0..6 {
            tr.observe(&[(0.0, 0.9)], 3.0 + k as f64);
        }
        // OnKey's skip to +30 s (allowing 0.2 s for the landing); it landed 0.05 s later still
        tr.skipped(10.0, (29.8, 1.5));
        tr.observe(&[(29.75, 0.9), (31.2, 0.3)], 13.0);
        assert!((tr.offset().unwrap() - 29.75).abs() < 0.01 && tr.confirmed(), "{:?}", tr.offset());
        // a near tie (a beat-off place nearly as good) waits for the usual evidence
        let mut tr = Tracker::new();
        tr.guess(0.0, 0.3, 0.0);
        tr.skipped(10.0, (29.8, 1.5));
        tr.observe(&[(29.75, 0.9), (30.2, 0.85)], 13.0);
        assert!(!tr.confirmed());
        // so does a match further from the guess, and a jump that isn't OnKey's
        let mut tr = Tracker::new();
        tr.guess(0.0, 0.3, 0.0);
        tr.skipped(10.0, (29.8, 1.5));
        tr.observe(&[(29.5, 0.9), (31.2, 0.3)], 13.0);
        assert!(!tr.confirmed());
        let mut tr = Tracker::new();
        tr.guess(0.0, 0.3, 0.0);
        tr.jumped(10.0, Some((29.8, 1.5)));
        tr.observe(&[(29.75, 0.9), (31.2, 0.3)], 13.0);
        assert!(!tr.confirmed());
    }

    #[test]
    fn a_single_stray_match_is_ignored() {
        let mut tr = Tracker::new();
        tr.guess(0.0, 0.3, 0.0);
        for k in 0..10 {
            let peaks: Vec<(f64, f32)> = if k == 5 { vec![(12.0, 0.9), (0.0, 0.7)] } else { vec![(0.0, 0.8)] };
            tr.observe(&peaks, 30.0 + k as f64);
        }
        assert!(tr.offset().unwrap().abs() < 0.01);
    }
}
