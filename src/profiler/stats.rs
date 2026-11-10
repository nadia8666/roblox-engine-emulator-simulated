//! The statistics a comparison's verdicts rest on (whatif.rs, `ree profile try`), kept apart so each is checked on its own:
//! a paired test of two variants' timed rounds with the interval of their difference, the noise an A/A pair (the same code
//! run twice) shows at a number of rounds, and how a figure grows with n, with a fixed cost each call fitted apart.
//!
//! A timed round runs every arm once, in a fresh process each, in an order of its own, so a round pairs two arms' runs that
//! met the same machine at about the same time. Whatever else runs on the machine only ever slows a run, often several
//! times over in stretches (a power-saving mode, a scheduler throttling background work), so a run far above what its
//! arm and its round lead one to expect is left out before anything is tested ([`unslowed`]).

/// A run above this many times what its round and its arm lead one to expect was slowed by the machine, not by the code. A
/// run's own noise is a few percent (a fresh process each: where its heap and code land); the machine's other work slows
/// runs by 5-30 %, stretches of them 2-25x, and sometimes every run for a while (another program's build), which a round
/// shares and a pair's ratio cancels. Measured on a shared desktop under load (`ree profile try` on identical variants):
/// past 1.3x the A/A noise at 9 rounds read 6-12 %, past 1.15x 2-5 % on a quiet stretch, with no identical pair called
/// apart either way; judged against the arm's fastest run alone, a stretch where every run was 50 % slower left 22 of 90
/// runs, so the expectation is the round's own level (below).
pub const SLOWED: f64 = 1.15;

/// The arms' runs with the slowed ones taken out (None). Arm k's run in round r is expected at a_k * L_r, its arm's level
/// times its round's, fitted by median polish on the logarithms (Tukey: the arms' and rounds' medians of what is left,
/// in turn, until they settle), which holds when arms ran in different rounds (rounds added for some pairs only, while
/// the machine's load changed): an arm's own median would mix its quiet rounds with its loaded ones. A round where every
/// run was slowed has a level above the rest, and its runs stay; a run above [`SLOWED`] times its expectation is left out.
/// `arms[k][r]` is arm k's figure in round r, None where it did not run or failed.
pub fn unslowed(arms: &[Vec<Option<f64>>]) -> Vec<Vec<Option<f64>>> {
    let median = |values: &mut Vec<f64>| -> f64 {
        if values.is_empty() {
            return 0.0;
        }
        values.sort_by(f64::total_cmp);
        let middle = values.len() / 2;
        if values.len() % 2 == 1 { values[middle] } else { (values[middle - 1] + values[middle]) / 2.0 }
    };
    let logs: Vec<Vec<Option<f64>>> = arms.iter().map(|arm| arm.iter().map(|value| value.filter(|value| *value > 0.0 && value.is_finite()).map(f64::ln)).collect()).collect();
    let rounds = arms.iter().map(Vec::len).max().unwrap_or(0);
    let (mut arm_level, mut round_level) = (vec![0.0f64; arms.len()], vec![0.0f64; rounds]);
    for _ in 0..10 {
        for (k, row) in logs.iter().enumerate() {
            let mut left: Vec<f64> = row.iter().enumerate().filter_map(|(r, value)| Some((*value)? - arm_level[k] - round_level[r])).collect();
            arm_level[k] += median(&mut left);
        }
        for (r, level) in round_level.iter_mut().enumerate() {
            let mut left: Vec<f64> = logs.iter().enumerate().filter_map(|(k, row)| Some(row.get(r).copied().flatten()? - arm_level[k] - *level)).collect();
            *level += median(&mut left);
        }
    }
    arms.iter()
        .zip(&logs)
        .enumerate()
        .map(|(k, (arm, row))| arm.iter().zip(row).enumerate().map(|(r, (value, log))| (*log)?.lt(&(arm_level[k] + round_level[r] + SLOWED.ln() + 1e-12)).then_some((*value)?)).collect())
        .collect()
}

/// Round by round, ln(b / a) over the rounds where both ran (slowed runs already taken out: [`unslowed`])
pub fn log_ratios(a: &[Option<f64>], b: &[Option<f64>]) -> Vec<f64> {
    a.iter().zip(b).filter_map(|(a, b)| Some(((*a)?, (*b)?))).filter(|(a, b)| *a > 0.0 && *b > 0.0).map(|(a, b)| (b / a).ln()).collect()
}

/// The chance, with no difference between two arms, that one wins `wins` or more of `n` rounds (a one-sided sign test)
pub fn sign_tail(wins: usize, n: usize) -> f64 {
    if wins == 0 {
        return 1.0;
    }
    if wins > n {
        return 0.0;
    }
    // (binomial coefficients in floating point: n stays below a few hundred)
    let mut total = 0.0;
    let mut coefficient = 1.0f64;
    for k in 0..=n {
        if k >= wins {
            total += coefficient;
        }
        coefficient = coefficient * (n - k) as f64 / (k + 1) as f64;
    }
    total / 2f64.powi(n as i32)
}

/// The fewest rounds of `n` one arm must win for the sign test to clear `alpha` (None: not even all n do)
pub fn wins_needed(n: usize, alpha: f64) -> Option<usize> {
    (1..=n).find(|wins| sign_tail(*wins, n) <= alpha)
}

/// The exact null distribution of the Wilcoxon signed-rank sum for n values: how many of the 2^n sign patterns give each sum
fn signed_rank_counts(n: usize) -> Vec<f64> {
    let max = n * (n + 1) / 2;
    let mut counts = vec![0.0f64; max + 1];
    counts[0] = 1.0;
    for rank in 1..=n {
        for sum in (rank..=max).rev() {
            counts[sum] += counts[sum - rank];
        }
    }
    counts
}

/// The largest sum c with P(sum <= c) <= `tail`, None when even a sum of 0 is likelier
fn signed_rank_critical(n: usize, tail: f64) -> Option<usize> {
    let counts = signed_rank_counts(n);
    let total = 2f64.powi(n as i32);
    let mut cumulative = 0.0;
    let mut critical = None;
    for (sum, count) in counts.iter().enumerate() {
        cumulative += count;
        if cumulative / total <= tail {
            critical = Some(sum);
        } else {
            break;
        }
    }
    critical
}

/// The difference two arms' rounds show: the Hodges-Lehmann estimate (the median of the Walsh averages, (d_i + d_j) / 2 for
/// i <= j), with its two-sided interval at `level` from the exact signed-rank distribution
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shift {
    pub estimate: f64,
    /// The interval; None when n is too small for the level (fewer than 6 values at 95 %)
    pub interval: Option<(f64, f64)>,
}

pub fn shift(d: &[f64], level: f64) -> Option<Shift> {
    if d.is_empty() {
        return None;
    }
    let mut walsh: Vec<f64> = Vec::with_capacity(d.len() * (d.len() + 1) / 2);
    for i in 0..d.len() {
        for j in i..d.len() {
            walsh.push((d[i] + d[j]) / 2.0);
        }
    }
    walsh.sort_by(f64::total_cmp);
    let count = walsh.len();
    let estimate = if count % 2 == 1 { walsh[count / 2] } else { (walsh[count / 2 - 1] + walsh[count / 2]) / 2.0 };
    let interval = signed_rank_critical(d.len(), (1.0 - level) / 2.0).map(|c| (walsh[c], walsh[count - 1 - c]));
    Some(Shift { estimate, interval })
}

/// The values at `q` of some samples, interpolated
pub fn quantile(values: &[f64], q: f64) -> Option<f64> {
    let mut sorted: Vec<f64> = values.iter().copied().filter(|value| value.is_finite()).collect();
    if sorted.is_empty() {
        return None;
    }
    sorted.sort_by(f64::total_cmp);
    let position = q.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let (low, high) = (position.floor() as usize, position.ceil() as usize);
    Some(sorted[low] + (sorted[high] - sorted[low]) * (position - low as f64))
}

/// How far the shift estimate of two runs of the same code reaches at n rounds: the `q` quantile of |estimate| over `draws`
/// samples of n drawn with replacement from `pool` (the A/A pairs' log ratios, slowed rounds out). A fixed seed: the same
/// pool gives the same level. None for a pool too small to draw from (under 4 values)
pub fn noise_level(pool: &[f64], n: usize, q: f64, draws: usize, seed: u64) -> Option<f64> {
    if pool.len() < 4 || n == 0 {
        return None;
    }
    let mut state = 0x9e37_79b9_7f4a_7c15u64 ^ seed.wrapping_mul(0xbf58_476d_1ce4_e5b9) ^ (n as u64).wrapping_mul(0x94d0_49bb_1331_11eb);
    let mut next = || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    };
    let mut estimates = Vec::with_capacity(draws);
    let mut sample = vec![0.0; n];
    for _ in 0..draws {
        for value in sample.iter_mut() {
            *value = pool[(next() % pool.len() as u64) as usize];
        }
        if let Some(found) = shift(&sample, 0.95) {
            estimates.push(found.estimate.abs());
        }
    }
    quantile(&estimates, q)
}

/// A paired verdict on two arms: whether b is clearly slower (or more) than a
#[derive(Clone, Debug, PartialEq)]
pub struct Paired {
    /// Rounds paired (both ran, neither slowed), and those b was above a in
    pub n: usize,
    pub wins: usize,
    /// The sign test's one-sided chance of that many wins with no difference
    pub p: f64,
    /// The level it had to clear: `alpha` on the first look, `later_alpha` once rounds were added for the pair
    pub alpha: f64,
    /// ln(b / a): the shift estimate and its 95 % interval
    pub estimate: f64,
    pub interval: Option<(f64, f64)>,
    /// How far two runs of the same code reach at n rounds (ln), and the floor below which no gap counts
    pub noise: Option<f64>,
    pub floor: f64,
    pub clear: bool,
}

/// The level of a paired verdict's first look (8 of 9 rounds clear it), and of a look after rounds were added for the
/// pair (stricter: a pair looked at again has had more chances to cross by luck)
pub const ALPHA: f64 = 0.02;
pub const LATER_ALPHA: f64 = 0.005;
/// The A/A noise quantile a gap must reach
pub const NOISE_QUANTILE: f64 = 0.99;
/// Bootstrap draws for the noise level
pub const NOISE_DRAWS: usize = 2000;
/// The fewest paired rounds a verdict is given on
pub const MIN_ROUNDS: usize = 6;

/// Whether b is clearly above a, from their paired rounds' log ratios `d` (ln(b / a)) and the A/A pool: the sign test clears
/// its level (`later` looks: [`LATER_ALPHA`]), the shift's 95 % interval lies above 0, and the shift estimate reaches both
/// the A/A noise at that many rounds and `floor` (ln of the smallest ratio that counts)
pub fn paired(d: &[f64], pool: &[f64], floor: f64, later: bool) -> Paired {
    let n = d.len();
    let wins = d.iter().filter(|value| **value > 0.0).count();
    let p = sign_tail(wins, n);
    let alpha = if later { LATER_ALPHA } else { ALPHA };
    let found = shift(d, 0.95);
    let (estimate, interval) = found.map_or((0.0, None), |found| (found.estimate, found.interval));
    let noise = noise_level(pool, n.max(1), NOISE_QUANTILE, NOISE_DRAWS, 0x5eed);
    let reach = noise.unwrap_or(f64::INFINITY).max(floor);
    let clear = n >= MIN_ROUNDS && p <= alpha && interval.is_some_and(|(low, _)| low > 0.0) && estimate >= reach;
    Paired { n, wins, p, alpha, estimate, interval, noise, floor, clear }
}

/// Whether two runs of the same code (an A/A pair) parted: their paired rounds clear the sign test one way or the other at
/// [`ALPHA`], the shift's interval leaves out 0 and the shift reaches `floor` (ln). Then something on the machine moved the
/// runs more than any code would, and a verdict at that size would read it as the code's
pub fn separated(d: &[f64], floor: f64) -> bool {
    let n = d.len();
    let Some(Shift { estimate, interval: Some((low, high)) }) = shift(d, 0.95) else { return false };
    if n < MIN_ROUNDS {
        return false;
    }
    let (up, down) = (d.iter().filter(|value| **value > 0.0).count(), d.iter().filter(|value| **value < 0.0).count());
    (sign_tail(up, n) <= ALPHA && low > 0.0 && estimate >= floor) || (sign_tail(down, n) <= ALPHA && high < 0.0 && -estimate >= floor)
}

/// How a figure grows with n: a fit of y = a + b * n^k (a fixed cost a call, a >= 0, and a part growing as n^k) by least
/// squares on the relative error, each segment's log-log slope, and the large-n exponent: the slope of the growing part
/// (y - a) between the two largest sizes, which a fixed cost does not bend
#[derive(Clone, Debug, PartialEq)]
pub struct Growth {
    /// The fitted exponent, fixed cost and coefficient (with two sizes no fixed cost is fitted: a = 0)
    pub k: f64,
    pub fixed: f64,
    pub coefficient: f64,
    /// The fit's largest relative error at a size
    pub error: f64,
    /// Each pair of neighbouring sizes: (n, next n, log-log slope of the figure)
    pub segments: Vec<(f64, f64, f64)>,
    /// The growing part's exponent between the two largest sizes (the headline: issues and the scaling column read it)
    pub large_n: f64,
    /// Whether a fixed cost was fitted (three sizes or more)
    pub fitted: bool,
}

/// The best (a, b) for one k, a >= 0 and b >= 0, weighting each point by 1 / y^2 (relative error); and its error sum
fn fit_at(points: &[(f64, f64)], k: f64) -> (f64, f64, f64) {
    let (mut sw, mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for &(n, y) in points {
        let (w, x) = (1.0 / (y * y), n.powf(k));
        sw += w;
        sx += w * x;
        sy += w * y;
        sxx += w * x * x;
        sxy += w * x * y;
    }
    let det = sw * sxx - sx * sx;
    let (mut a, mut b) = if det.abs() > 1e-300 { ((sy * sxx - sx * sxy) / det, (sw * sxy - sx * sy) / det) } else { (0.0, if sxx > 0.0 { sxy / sxx } else { 0.0 }) };
    if a < 0.0 {
        a = 0.0;
        b = if sxx > 0.0 { sxy / sxx } else { 0.0 };
    }
    if b < 0.0 {
        b = 0.0;
        a = sy / sw;
    }
    let error = points.iter().map(|&(n, y)| ((y - a - b * n.powf(k)) / y).powi(2)).sum();
    (a, b, error)
}

pub fn growth(points: &[(f64, f64)]) -> Option<Growth> {
    let mut points: Vec<(f64, f64)> = points.iter().copied().filter(|(n, y)| *n > 0.0 && *y > 0.0 && n.is_finite() && y.is_finite()).collect();
    points.sort_by(|a, b| a.0.total_cmp(&b.0));
    points.dedup_by(|a, b| a.0 == b.0);
    if points.len() < 2 {
        return None;
    }
    let segments: Vec<(f64, f64, f64)> = points.windows(2).map(|pair| (pair[0].0, pair[1].0, (pair[1].1 / pair[0].1).ln() / (pair[1].0 / pair[0].0).ln())).collect();
    let last = segments.last().map(|s| s.2).unwrap_or(0.0);
    if points.len() == 2 {
        let (n, y) = points[1];
        return Some(Growth { k: last, fixed: 0.0, coefficient: y / n.powf(last), error: 0.0, segments, large_n: last, fitted: false });
    }
    // k on a grid, then refined around the best
    let mut best = (0.0, 0.0, 0.0, f64::INFINITY);
    let mut k = 0.0;
    while k <= 4.0 + 1e-9 {
        let (a, b, error) = fit_at(&points, k);
        if error < best.3 {
            best = (k, a, b, error);
        }
        k += 0.01;
    }
    let (mut low, mut high) = ((best.0 - 0.01f64).max(0.0), best.0 + 0.01);
    for _ in 0..60 {
        let (m1, m2) = (low + (high - low) / 3.0, high - (high - low) / 3.0);
        if fit_at(&points, m1).2 <= fit_at(&points, m2).2 {
            high = m2;
        } else {
            low = m1;
        }
    }
    let refined = (low + high) / 2.0;
    let (a, b, error) = fit_at(&points, refined);
    let (k, a, b) = if error <= best.3 { (refined, a, b) } else { (best.0, best.1, best.2) };
    let fit_error = points.iter().map(|&(n, y)| ((y - a - b * n.powf(k)) / y).abs()).fold(0.0, f64::max);
    // the growing part between the two largest sizes (the fit's own k when the fixed cost takes all of the smaller one)
    let (n1, y1) = points[points.len() - 2];
    let (n2, y2) = points[points.len() - 1];
    // (a figure that does not grow: the fixed cost is all of it)
    if b * n2.powf(k) <= 1e-9 * y2 {
        return Some(Growth { k: 0.0, fixed: a, coefficient: 0.0, error: fit_error, segments, large_n: 0.0, fitted: true });
    }
    let large_n = if y1 - a > 0.0 && y2 - a > 0.0 { ((y2 - a) / (y1 - a)).ln() / (n2 / n1).ln() } else { k };
    Some(Growth { k, fixed: a, coefficient: b, error: fit_error, segments, large_n, fitted: true })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sign_test_needs_eight_of_nine_rounds() {
        assert!((sign_tail(8, 9) - 10.0 / 512.0).abs() < 1e-12);
        assert!((sign_tail(9, 9) - 1.0 / 512.0).abs() < 1e-12);
        assert_eq!(sign_tail(0, 9), 1.0);
        assert_eq!(wins_needed(9, ALPHA), Some(8));
        assert_eq!(wins_needed(9, LATER_ALPHA), Some(9));
        assert_eq!(wins_needed(5, ALPHA), None);
        assert_eq!(wins_needed(15, ALPHA), Some(12));
    }

    /// The Hodges-Lehmann shift and its interval: the textbook case of n = 9 takes the 6th Walsh average from each end
    /// (signed-rank critical value 5 at 2.5 % a side)
    #[test]
    fn a_shift_has_an_exact_interval() {
        assert_eq!(signed_rank_critical(9, 0.025), Some(5));
        assert_eq!(signed_rank_critical(5, 0.025), None);
        let d = [0.10, 0.12, 0.08, 0.11, 0.09, 0.13, 0.10, 0.07, 0.12];
        let found = shift(&d, 0.95).unwrap();
        assert!((found.estimate - 0.10).abs() < 0.006, "{found:?}");
        let (low, high) = found.interval.unwrap();
        assert!(low > 0.07 && low < 0.10 && high > 0.10 && high < 0.13, "{low} {high}");
        assert!(shift(&[0.1, 0.2, 0.3], 0.95).unwrap().interval.is_none(), "too few for 95 %");
    }

    /// A run slowed alone is left out; rounds the machine slowed as a whole stay, since a pair's ratio cancels them (the
    /// case that judging against an arm's fastest run got wrong: rounds 3 on 50 % slower for every arm)
    #[test]
    fn slowed_runs_are_left_out_of_the_pairs() {
        let a = vec![Some(1.0), Some(1.01), Some(5.0), Some(0.99), None, Some(1.5), Some(1.52)];
        let b = vec![Some(1.1), Some(1.12), Some(1.1), Some(3.0), Some(1.0), Some(1.65), Some(1.66)];
        let c = vec![Some(2.0), Some(2.02), Some(2.01), Some(1.99), Some(2.0), Some(3.0), Some(3.03)];
        let clean = unslowed(&[a, b, c]);
        assert_eq!(clean[0][2], None, "a slowed alone in round 2");
        assert_eq!(clean[1][3], None, "b slowed alone in round 3");
        assert_eq!(clean[0][4], None, "a failed in round 4");
        assert!(clean[0][5].is_some() && clean[1][5].is_some() && clean[2][6].is_some(), "rounds 5 and 6 slowed every arm: they stay");
        let d = log_ratios(&clean[0], &clean[1]);
        assert_eq!(d.len(), 4);
        assert!(d.iter().all(|d| (d - 1.1f64.ln()).abs() < 0.02), "{d:?}");
    }

    /// Identical arms never clear; a gap of 10 % in every round does
    #[test]
    fn a_paired_verdict_wants_rounds_and_a_gap_beyond_the_noise() {
        // an A/A pool of +-2 % noise
        let pool: Vec<f64> = (0..36).map(|i| ((i * 37 % 17) as f64 - 8.0) * 0.005).collect();
        let gap: Vec<f64> = (0..9).map(|i| 0.10 + ((i * 5 % 7) as f64 - 3.0) * 0.005).collect();
        let verdict = paired(&gap, &pool, 0.01f64.ln_1p(), false);
        assert!(verdict.clear, "{verdict:?}");
        assert_eq!(verdict.wins, 9);
        // the same values with no gap: half won, no verdict
        let none: Vec<f64> = gap.iter().map(|value| value - 0.10).collect();
        assert!(!paired(&none, &pool, 0.01f64.ln_1p(), false).clear);
        // 7 of 9 won: below the sign test's level however large the gap
        let mut seven = gap.clone();
        seven[0] = -0.05;
        seven[1] = -0.04;
        assert!(!paired(&seven, &pool, 0.01f64.ln_1p(), false).clear);
        // a gap below the floor counts for nothing
        let small: Vec<f64> = gap.iter().map(|value| value - 0.095).collect();
        assert!(!paired(&small, &pool, 0.02f64.ln_1p(), false).clear);
        // too few rounds
        assert!(!paired(&gap[..5], &pool, 0.01f64.ln_1p(), false).clear);
    }

    /// A fixed cost each call bends a log-log fit without one; the fit with it finds n, n log n and n^2
    #[test]
    fn growth_fits_a_fixed_cost_apart() {
        let sizes = [16.0, 64.0, 256.0, 1024.0];
        let linear: Vec<(f64, f64)> = sizes.iter().map(|&n| (n, 2000.0 + 3.0 * n)).collect();
        let found = growth(&linear).unwrap();
        assert!((found.k - 1.0).abs() < 0.02 && (found.large_n - 1.0).abs() < 0.02, "{found:?}");
        assert!((found.fixed - 2000.0).abs() < 40.0, "{found:?}");
        assert!(found.segments[0].2 < 0.1, "the first segment is nearly flat: {:?}", found.segments);
        let sort: Vec<(f64, f64)> = sizes.iter().map(|&n| (n, 500.0 + 4.0 * n * n.log2())).collect();
        let found = growth(&sort).unwrap();
        assert!(found.large_n > 1.05 && found.large_n < 1.2, "{found:?}");
        let quadratic: Vec<(f64, f64)> = sizes.iter().map(|&n| (n, 2000.0 + 5.0 * n + 0.5 * n * n)).collect();
        let found = growth(&quadratic).unwrap();
        assert!(found.large_n > 1.9 && found.large_n < 2.05, "{found:?}");
        // two sizes: their slope, no fixed cost
        let found = growth(&[(10.0, 1.0), (100.0, 10.0)]).unwrap();
        assert!(!found.fitted && (found.large_n - 1.0).abs() < 1e-12);
        assert!(growth(&[(10.0, 1.0)]).is_none());
        // a flat figure grows as n^0
        let flat: Vec<(f64, f64)> = sizes.iter().map(|&n| (n, 7.0)).collect();
        assert!(growth(&flat).unwrap().large_n.abs() < 1e-9);
    }

    /// An O(n²) scan with a fixed cost and a linear part a call (3.76 us + 25.7 ns an element + 0.79 ns a pair): a log-log
    /// fit without a fixed cost reads n^1.15 over n = 10 to 1,000, under the 1.3 an issue starts at, while it grows as
    /// n^1.76 between the two largest sizes; the exponent at large n, the growing part's slope, reads quadratic
    #[test]
    fn a_quadratic_scan_reads_quadratic_at_large_n() {
        let points = [(10.0, 0.0041), (100.0, 0.0142), (1000.0, 0.816)];
        let logs: Vec<(f64, f64)> = points.iter().map(|(n, y)| (f64::ln(*n), f64::ln(*y))).collect();
        let (mx, my) = (logs.iter().map(|p| p.0).sum::<f64>() / 3.0, logs.iter().map(|p| p.1).sum::<f64>() / 3.0);
        let loglog = logs.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f64>() / logs.iter().map(|p| (p.0 - mx).powi(2)).sum::<f64>();
        assert!((loglog - 1.15).abs() < 0.01, "the old fit: {loglog}");
        let found = growth(&points).unwrap();
        assert!((found.segments[1].2 - 1.76).abs() < 0.01, "{found:?}");
        // (three sizes fit a + b n^k exactly: k 1.90 with a fixed 3.97 us)
        assert!((found.large_n - 1.90).abs() < 0.01 && (found.fixed - 0.00397).abs() < 0.0001, "a quadratic scan at large n: {found:?}");
    }

    #[test]
    fn the_noise_level_follows_the_pool() {
        let pool: Vec<f64> = (0..40).map(|i| ((i * 37 % 17) as f64 - 8.0) * 0.005).collect();
        let at9 = noise_level(&pool, 9, 0.99, 2000, 1).unwrap();
        let at27 = noise_level(&pool, 27, 0.99, 2000, 1).unwrap();
        assert!(at9 > at27 && at9 < 0.04, "{at9} {at27}");
        assert_eq!(noise_level(&pool, 9, 0.99, 2000, 1), Some(at9), "the same pool, the same level");
        assert!(noise_level(&pool[..3], 9, 0.99, 100, 1).is_none());
    }
}
