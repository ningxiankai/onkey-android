//! CTC forced alignment: given a model's per-frame log-probabilities over characters and the text that was
//! sung, find when each character was sung (the most likely path through the text, blanks allowed between
//! characters). Same result as torchaudio's forced_align + merge_tokens.

/// Per token: (first frame, frame after the last, mean probability over its frames).
pub fn align(logp: &[f32], classes: usize, tokens: &[usize], blank: usize) -> Option<Vec<(usize, usize, f32)>> {
    let t_len = logp.len() / classes;
    let l = tokens.len();
    if l == 0 || t_len < l {
        return None;
    }
    let s_len = 2 * l + 1;
    let ext = |s: usize| if s % 2 == 0 { blank } else { tokens[s / 2] };
    let lp = |t: usize, c: usize| logp[t * classes + c];
    let neg = f32::NEG_INFINITY;
    let mut prev = vec![neg; s_len];
    let mut cur = vec![neg; s_len];
    let mut back = vec![0u8; t_len * s_len];        // 0 stay, 1 from s-1, 2 from s-2
    prev[0] = lp(0, blank);
    prev[1] = lp(0, ext(1));
    for t in 1..t_len {
        for s in 0..s_len {
            let mut best = prev[s];
            let mut how = 0u8;
            if s >= 1 && prev[s - 1] > best {
                best = prev[s - 1];
                how = 1;
            }
            if s >= 2 && ext(s) != blank && ext(s) != ext(s - 2) && prev[s - 2] > best {
                best = prev[s - 2];
                how = 2;
            }
            cur[s] = if best == neg { neg } else { best + lp(t, ext(s)) };
            back[t * s_len + s] = how;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    let mut s = if prev[s_len - 1] >= prev[s_len - 2] { s_len - 1 } else { s_len - 2 };
    if prev[s] == neg {
        return None;
    }
    let mut path = vec![0usize; t_len];
    for t in (0..t_len).rev() {
        path[t] = s;
        if t > 0 {
            s -= back[t * s_len + s] as usize;
        }
    }
    let mut out = vec![(usize::MAX, 0usize, 0f32, 0usize); l];
    for (t, &s) in path.iter().enumerate() {
        if s % 2 == 1 {
            let j = s / 2;
            let o = &mut out[j];
            o.0 = o.0.min(t);
            o.1 = t + 1;
            o.2 += lp(t, tokens[j]).exp();
            o.3 += 1;
        }
    }
    Some(out.into_iter().map(|(a, b, sc, n)| (a, b, if n > 0 { sc / n as f32 } else { 0.0 })).collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn aligns_a_simple_sequence() {
        // 3 classes (blank, A, B); frames: blank, A, A, blank, B
        let p = |v: [f32; 3]| v.map(|x| x.max(1e-6).ln());
        let rows = [p([0.9, 0.05, 0.05]), p([0.1, 0.8, 0.1]), p([0.1, 0.8, 0.1]), p([0.8, 0.1, 0.1]), p([0.1, 0.1, 0.8])];
        let logp: Vec<f32> = rows.iter().flatten().copied().collect();
        let a = super::align(&logp, 3, &[1, 2], 0).unwrap();
        assert_eq!((a[0].0, a[0].1), (1, 3));
        assert_eq!((a[1].0, a[1].1), (4, 5));
    }
}
