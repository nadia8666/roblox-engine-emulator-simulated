//! Animation curves: the keys of a FloatCurve, RotationCurve and MarkerCurve, how they are stored
//! (the `ValuesAndTimes` property), and their value at a time. Measured in Studio 0.747
//! (2026-11-11, curves built by script; docs/engine.md "Animation curves" holds the numbers):
//!
//! - A key's time is a whole number of ticks, 2400 a second: the time asked for, rounded to the
//!   nearest tick (0.0004 s is tick 1, 1.0003 s tick 2401); a time that is not a number or is past
//!   the 32-bit range is the lowest tick. A curve holds one key a tick, in time order.
//! - A FloatCurve's value: before the first key and after the last, that key's value. Between two
//!   keys the earlier one's mode decides. Constant holds it. Linear is `a * (1 - s) + b * s` in
//!   float32, `s = (t - ta) / (tb - ta)`. Cubic is the Hermite basis in float32, with the earlier
//!   key's right tangent and the later key's left one as slopes (value a second), each times the
//!   span: `h00 * a + h10 * (dt * m0) + h01 * b + h11 * (dt * m1)`, summed left to right. Every
//!   value of curve_golden/float_curve.luau and curve_golden/tangents.luau agrees with
//!   `GetValueAtTime` to the last bit.
//! - The keys around a time are found by the time's tick, and the way between them by the time
//!   itself: less than half a tick before a key the value is that key's already.
//! - A tangent that is not set ([`right_tangent`], [`left_tangent`]): the slope the curve has on
//!   the key's other side. That is the neighbouring Linear segment's slope, 0 beside a Constant
//!   one, and the key's other tangent when that one is set and the segment there is Cubic. With
//!   neither set between two Cubic segments it is the mean of the two segments' slopes, scaled
//!   down as the key's value nears a neighbour's ([`auto_tangent`]) and 0 at a peak or a dip. A
//!   first key's right tangent and a last key's left one are 0 (a last key's set right tangent is
//!   still taken for its left).
//! - A RotationCurve turns along the shortest arc (slerp): Linear by `s`, Cubic by the same
//!   Hermite basis run from 0 to 1 with the tangents as slopes of that fraction (a second), not
//!   clamped. Its unset tangents follow the FloatCurve rule with every key one apart in value;
//!   of those only the mean between two Cubic segments and the slope beside a Linear one were
//!   measured.
//! - EulerRotationCurve's rotation is `CFrame.fromEulerAngles(x, y, z, RotationOrder)`.
//!
//! The stored form was read from files Studio wrote of curves built for this (SerializationService),
//! never from Roblox's code. Little-endian:
//!
//! - FloatCurve: u32 2, u32 count, a key each (u8 mode, u8 flags: 1 left tangent set, 2 right
//!   tangent set, f32 value, f32 left, f32 right), u32 1, u32 count, an i32 tick each. An unset
//!   tangent's field holds a filler (Studio writes a resolved slope there): only the flags say
//!   what is set.
//! - RotationCurve: u32 1, u32 count, a key each (u8: mode in the low two bits, 4 left tangent
//!   set, 8 right tangent set; f32 x y z w; f32 left; f32 right), then the ticks as above.
//! - MarkerCurve: u32 2, u32 count, a zero-terminated value each, then the ticks as above.

use glam::{Mat3, Quat};

pub const TICKS_PER_SECOND: f32 = 2400.0;

// Enum.KeyInterpolationMode
pub const CONSTANT: u8 = 0;
pub const LINEAR: u8 = 1;
pub const CUBIC: u8 = 2;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FloatKey {
    pub tick: i32,
    pub value: f32,
    pub mode: u8,
    pub left: Option<f32>,
    pub right: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RotationKey {
    pub tick: i32,
    pub rotation: Quat,
    pub mode: u8,
    pub left: Option<f32>,
    pub right: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MarkerKey {
    pub tick: i32,
    pub value: Vec<u8>,
}

/// A key of any curve: its tick.
pub trait Timed {
    fn tick(&self) -> i32;
}

impl Timed for FloatKey {
    fn tick(&self) -> i32 {
        self.tick
    }
}

impl Timed for RotationKey {
    fn tick(&self) -> i32 {
        self.tick
    }
}

impl Timed for MarkerKey {
    fn tick(&self) -> i32 {
        self.tick
    }
}

/// The tick of a time in seconds.
pub fn tick_of(time: f32) -> i32 {
    let ticks = (time as f64 * TICKS_PER_SECOND as f64).round();
    if ticks.is_nan() || ticks < i32::MIN as f64 || ticks > i32::MAX as f64 {
        return i32::MIN;
    }
    ticks as i32
}

/// A tick's time in seconds.
pub fn time_of(tick: i32) -> f32 {
    tick as f32 / TICKS_PER_SECOND
}

/// InsertKey: `key` replaces the key at its tick (false) or goes in at its place in time (true);
/// with its index from 0.
pub fn insert<K: Timed>(keys: &mut Vec<K>, key: K) -> (bool, usize) {
    match keys.binary_search_by_key(&key.tick(), Timed::tick) {
        Ok(index) => {
            keys[index] = key;
            (false, index)
        }
        Err(index) => {
            keys.insert(index, key);
            (true, index)
        }
    }
}

/// SetKeys: the keys in time order (keys of one time in the order given), without the ones that come
/// less than a tick after the key kept before them. `keys` holds each key with the time it was
/// given, before rounding, and the float32 difference of those times is what counts: 1.0 and 1.0003
/// leave one key (1.0003 alone is tick 2401), and so do 1.0 and 1.0 + 1/2400, whose float32
/// difference falls short of 1/2400. Measured: 201 keys at k/2400 s leave 113, at 7 + k/2400 s 164
/// and at 700 + k/2400 s 166.
pub fn set<K: Timed>(mut keys: Vec<(f32, K)>) -> Vec<K> {
    keys.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut kept: Vec<K> = Vec::with_capacity(keys.len());
    let mut last = f32::NEG_INFINITY;
    for (time, key) in keys {
        let after_last = kept.last().is_none_or(|before| key.tick() > before.tick());
        if after_last && (kept.is_empty() || time - last >= 1.0 / TICKS_PER_SECOND) {
            last = time;
            kept.push(key);
        }
    }
    kept
}

/// RemoveKeyAtIndex: takes up to `count` keys out from index `start` (from 1); how many.
pub fn remove<K>(keys: &mut Vec<K>, start: usize, count: usize) -> usize {
    if start == 0 || start > keys.len() {
        return 0;
    }
    let end = (start - 1).saturating_add(count).min(keys.len());
    keys.drain(start - 1..end).count()
}

/// GetKeyIndicesAtTime: the key at or before `time`'s tick and the one after it, from 1; kept inside
/// the curve, so the first two before the first key and the last two from the last key on. (0, 0)
/// without keys and (1, 1) with one.
pub fn indices_at<K: Timed>(keys: &[K], time: f32) -> (usize, usize) {
    match keys.len() {
        0 => (0, 0),
        1 => (1, 1),
        count => {
            let tick = tick_of(time);
            let before = keys.partition_point(|key| key.tick() <= tick).clamp(1, count - 1);
            (before, before + 1)
        }
    }
}

/// The keys around `time`: Err with the value's index when it holds one key's value (outside the
/// keys), else the earlier key's index and how far `time` is to the next (0 to 1) with the span in
/// seconds. The keys are found by `time`'s tick and the fraction by `time` itself, held at 0: a time
/// less than half a tick before a key has that key's value already, and half a tick before the
/// last key it is still on the way there (measured: keys 0, 10, 30 at 1, 2, 3 s give 9.997 at
/// 1.9997 s, 10 at 1.9998 s and 29.998 at 2.9999 s).
fn segment<K: Timed>(keys: &[K], time: f32) -> Result<(usize, f32, f32), usize> {
    let tick = tick_of(time);
    let next = keys.partition_point(|key| key.tick() <= tick);
    if next == 0 {
        return Err(0);
    }
    if keys.len() == 1 {
        return Err(0);
    }
    let index = (next - 1).min(keys.len() - 2);
    let (from, to) = (time_of(keys[index].tick()), time_of(keys[index + 1].tick()));
    let span = to - from;
    let fraction = (time - from) / span;
    if fraction >= 1.0 {
        return Err(index + 1);
    }
    Ok((index, if fraction < 0.0 { 0.0 } else { fraction }, span))
}

/// The cubic Hermite basis at `s`: the weights of the first value, its tangent, the second value
/// and its tangent.
fn hermite(s: f32) -> (f32, f32, f32, f32) {
    let s2 = s * s;
    let s3 = s2 * s;
    (2.0 * s3 - 3.0 * s2 + 1.0, s3 - 2.0 * s2 + s, -2.0 * s3 + 3.0 * s2, s3 - s2)
}

fn slope(keys: &[FloatKey], from: usize) -> f32 {
    (keys[from + 1].value - keys[from].value) / (time_of(keys[from + 1].tick) - time_of(keys[from].tick))
}

/// The tangent of key `index`, between two Cubic segments and with neither tangent set: the mean of
/// the two slopes times `min(1, 4 * min(r, 1 - r))`, `r` how far the key's value is from the key
/// before's to the key after's, `(v1 - v0) / (v2 - v0)`; 0 where the key is no nearer than a
/// neighbour (a peak, a dip, a level step). All in float32, the slopes over the difference of the
/// keys' times: of the orderings and precisions tried, the ones that give this give all 240 tangents
/// Studio stored for random keys bit for bit (it writes the tangent it resolved into the unset
/// fields), and no other gives more than 51 of the first 60. Slopes 3 and 0.5 over 1 s and 2 s give
/// 1.75, 10 and 0.1 give 0.396078, 0.1 and 10 give 0.100498, -2 and -4 give -2.4, 2 and 2 give 2.
pub fn auto_tangent(keys: &[FloatKey], index: usize) -> f32 {
    let (before, value, after) = (keys[index - 1].value, keys[index].value, keys[index + 1].value);
    let r = (value - before) / (after - before);
    let factor = 4.0 * r.min(1.0 - r);
    // (no value between the neighbours', or all three the same: not a number)
    if factor.is_nan() || factor <= 0.0 {
        return 0.0;
    }
    (slope(keys, index - 1) + slope(keys, index)) * 0.5 * factor.min(1.0)
}

/// The slope a Cubic segment leaves key `index` with (the key is Cubic and not the last).
pub fn right_tangent(keys: &[FloatKey], index: usize) -> f32 {
    let key = &keys[index];
    if let Some(right) = key.right {
        return right;
    }
    if index == 0 {
        return 0.0;
    }
    match keys[index - 1].mode {
        LINEAR => slope(keys, index - 1),
        CUBIC => key.left.unwrap_or_else(|| auto_tangent(keys, index)),
        _ => 0.0,
    }
}

/// The slope a Cubic segment reaches key `index` with (the key before it is Cubic).
pub fn left_tangent(keys: &[FloatKey], index: usize) -> f32 {
    let key = &keys[index];
    if let Some(left) = key.left {
        return left;
    }
    if let Some(right) = key.right.filter(|_| key.mode == CUBIC) {
        return right;
    }
    if index + 1 == keys.len() {
        return 0.0;
    }
    match key.mode {
        LINEAR => slope(keys, index),
        CUBIC => auto_tangent(keys, index),
        _ => 0.0,
    }
}

/// FloatCurve:GetValueAtTime; None without keys.
pub fn float_at(keys: &[FloatKey], time: f32) -> Option<f32> {
    if keys.is_empty() {
        return None;
    }
    let (index, s, span) = match segment(keys, time) {
        Ok(found) => found,
        Err(index) => return Some(keys[index].value),
    };
    let (a, b) = (&keys[index], &keys[index + 1]);
    Some(match a.mode {
        LINEAR => a.value * (1.0 - s) + b.value * s,
        CUBIC => {
            let (h00, h10, h01, h11) = hermite(s);
            h00 * a.value + h10 * (span * right_tangent(keys, index)) + h01 * b.value + h11 * (span * left_tangent(keys, index + 1))
        }
        _ => a.value,
    })
}

fn rate(keys: &[RotationKey], from: usize) -> f32 {
    1.0 / (time_of(keys[from + 1].tick) - time_of(keys[from].tick))
}

fn rotation_right_tangent(keys: &[RotationKey], index: usize) -> f32 {
    let key = &keys[index];
    if let Some(right) = key.right {
        return right;
    }
    if index == 0 {
        return 0.0;
    }
    match keys[index - 1].mode {
        LINEAR => rate(keys, index - 1),
        CUBIC => key.left.unwrap_or_else(|| (rate(keys, index - 1) + rate(keys, index)) * 0.5),
        _ => 0.0,
    }
}

fn rotation_left_tangent(keys: &[RotationKey], index: usize) -> f32 {
    let key = &keys[index];
    if let Some(left) = key.left {
        return left;
    }
    if let Some(right) = key.right.filter(|_| key.mode == CUBIC) {
        return right;
    }
    if index + 1 == keys.len() {
        return 0.0;
    }
    match key.mode {
        LINEAR => rate(keys, index),
        CUBIC => (rate(keys, index - 1) + rate(keys, index)) * 0.5,
        _ => 0.0,
    }
}

/// The rotation `fraction` of the way from `a` to `b` along the shortest arc; not clamped (a Cubic
/// segment's fraction can leave 0 to 1).
fn turn(a: Quat, b: Quat, fraction: f32) -> Quat {
    let b = if a.dot(b) < 0.0 { -b } else { b };
    let cosine = a.dot(b).clamp(-1.0, 1.0);
    let angle = cosine.acos();
    let sine = angle.sin();
    if sine < 1e-6 {
        return (a * (1.0 - fraction) + b * fraction).normalize();
    }
    (a * (((1.0 - fraction) * angle).sin() / sine) + b * ((fraction * angle).sin() / sine)).normalize()
}

/// RotationCurve:GetValueAtTime; None without keys.
pub fn rotation_at(keys: &[RotationKey], time: f32) -> Option<Quat> {
    if keys.is_empty() {
        return None;
    }
    let (index, s, span) = match segment(keys, time) {
        Ok(found) => found,
        Err(index) => return Some(keys[index].rotation),
    };
    let (a, b) = (&keys[index], &keys[index + 1]);
    Some(match a.mode {
        LINEAR => turn(a.rotation, b.rotation, s),
        CUBIC => {
            let (_, h10, h01, h11) = hermite(s);
            let fraction = h10 * (span * rotation_right_tangent(keys, index)) + h01 + h11 * (span * rotation_left_tangent(keys, index + 1));
            turn(a.rotation, b.rotation, fraction)
        }
        _ => a.rotation,
    })
}

/// The rotation of three Euler angles in an Enum.RotationOrder (its value: XYZ 0, XZY 1, YZX 2,
/// YXZ 3, ZXY 4, ZYX 5), as `CFrame.fromEulerAngles` gives it (the same float32 arithmetic).
pub fn euler_matrix(x: f32, y: f32, z: f32, order: u32) -> Mat3 {
    let name = ["XYZ", "XZY", "YZX", "YXZ", "ZXY", "ZYX"].get(order as usize).copied().unwrap_or("XYZ");
    let rows = lune_roblox::datatypes::types::float32::from_euler(name, x, y, z);
    Mat3::from_cols_array_2d(&rows).transpose()
}

/// [`euler_matrix`] as a quaternion.
pub fn euler(x: f32, y: f32, z: f32, order: u32) -> Quat {
    Quat::from_mat3(&euler_matrix(x, y, z, order)).normalize()
}

// The stored form

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take(&mut self, count: usize) -> Option<&[u8]> {
        if self.0.len() < count {
            return None;
        }
        let (head, rest) = self.0.split_at(count);
        self.0 = rest;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|bytes| bytes[0])
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|bytes| u32::from_le_bytes(bytes.try_into().expect("four bytes")))
    }

    fn f32(&mut self) -> Option<f32> {
        self.u32().map(f32::from_bits)
    }

    /// A count of items at least `size` bytes each (a count the bytes cannot hold reads nothing).
    fn count(&mut self, size: usize) -> Option<usize> {
        let count = self.u32()? as usize;
        (count.checked_mul(size)? <= self.0.len()).then_some(count)
    }

    /// The tick block after `count` keys.
    fn ticks(&mut self, count: usize) -> Option<Vec<i32>> {
        self.u32()?;
        if self.count(4)? != count {
            return None;
        }
        (0..count).map(|_| self.u32().map(|bits| bits as i32)).collect()
    }
}

fn write_ticks(out: &mut Vec<u8>, ticks: impl ExactSizeIterator<Item = i32>) {
    out.extend(1u32.to_le_bytes());
    out.extend((ticks.len() as u32).to_le_bytes());
    for tick in ticks {
        out.extend(tick.to_le_bytes());
    }
}

/// A FloatCurve's keys; none when `bytes` is not the stored form.
pub fn decode_floats(bytes: &[u8]) -> Vec<FloatKey> {
    let mut reader = Reader(bytes);
    let read = |reader: &mut Reader| {
        reader.u32()?;
        let count = reader.count(14)?;
        let mut keys = Vec::with_capacity(count);
        for _ in 0..count {
            let (mode, flags, value, left, right) = (reader.u8()?, reader.u8()?, reader.f32()?, reader.f32()?, reader.f32()?);
            keys.push(FloatKey { tick: 0, value, mode: mode.min(CUBIC), left: (flags & 1 != 0).then_some(left), right: (flags & 2 != 0).then_some(right) });
        }
        for (key, tick) in keys.iter_mut().zip(reader.ticks(count)?) {
            key.tick = tick;
        }
        Some(keys)
    };
    in_order(read(&mut reader).unwrap_or_default())
}

pub fn encode_floats(keys: &[FloatKey]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + keys.len() * 18);
    out.extend(2u32.to_le_bytes());
    out.extend((keys.len() as u32).to_le_bytes());
    for key in keys {
        out.push(key.mode);
        out.push(key.left.is_some() as u8 | (key.right.is_some() as u8) << 1);
        out.extend(key.value.to_le_bytes());
        out.extend(key.left.unwrap_or(0.0).to_le_bytes());
        out.extend(key.right.unwrap_or(0.0).to_le_bytes());
    }
    write_ticks(&mut out, keys.iter().map(|key| key.tick));
    out
}

/// A RotationCurve's keys; none when `bytes` is not the stored form.
pub fn decode_rotations(bytes: &[u8]) -> Vec<RotationKey> {
    let mut reader = Reader(bytes);
    let read = |reader: &mut Reader| {
        reader.u32()?;
        let count = reader.count(25)?;
        let mut keys = Vec::with_capacity(count);
        for _ in 0..count {
            let head = reader.u8()?;
            let (x, y, z, w, left, right) = (reader.f32()?, reader.f32()?, reader.f32()?, reader.f32()?, reader.f32()?, reader.f32()?);
            let rotation = Quat::from_xyzw(x, y, z, w);
            keys.push(RotationKey {
                tick: 0,
                rotation: if rotation.length_squared() > 1e-12 && rotation.is_finite() { rotation.normalize() } else { Quat::IDENTITY },
                mode: (head & 3).min(CUBIC),
                left: (head & 4 != 0).then_some(left),
                right: (head & 8 != 0).then_some(right),
            });
        }
        for (key, tick) in keys.iter_mut().zip(reader.ticks(count)?) {
            key.tick = tick;
        }
        Some(keys)
    };
    in_order(read(&mut reader).unwrap_or_default())
}

pub fn encode_rotations(keys: &[RotationKey]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + keys.len() * 29);
    out.extend(1u32.to_le_bytes());
    out.extend((keys.len() as u32).to_le_bytes());
    for key in keys {
        out.push(key.mode | (key.left.is_some() as u8) << 2 | (key.right.is_some() as u8) << 3);
        for part in [key.rotation.x, key.rotation.y, key.rotation.z, key.rotation.w, key.left.unwrap_or(0.0), key.right.unwrap_or(0.0)] {
            out.extend(part.to_le_bytes());
        }
    }
    write_ticks(&mut out, keys.iter().map(|key| key.tick));
    out
}

/// A MarkerCurve's markers; none when `bytes` is not the stored form.
pub fn decode_markers(bytes: &[u8]) -> Vec<MarkerKey> {
    let mut reader = Reader(bytes);
    let read = |reader: &mut Reader| {
        reader.u32()?;
        let count = reader.count(1)?;
        let mut keys = Vec::with_capacity(count);
        for _ in 0..count {
            let end = reader.0.iter().position(|&byte| byte == 0)?;
            let value = reader.take(end)?.to_vec();
            reader.u8()?;
            keys.push(MarkerKey { tick: 0, value });
        }
        for (key, tick) in keys.iter_mut().zip(reader.ticks(count)?) {
            key.tick = tick;
        }
        Some(keys)
    };
    in_order(read(&mut reader).unwrap_or_default())
}

pub fn encode_markers(keys: &[MarkerKey]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(2u32.to_le_bytes());
    out.extend((keys.len() as u32).to_le_bytes());
    for key in keys {
        // (a value ends at its first zero byte, as the stored form has it)
        out.extend(key.value.iter().take_while(|&&byte| byte != 0));
        out.push(0);
    }
    write_ticks(&mut out, keys.iter().map(|key| key.tick));
    out
}

/// Keys as a curve holds them: in time order, one a tick (a file's keys are, unless it was made by
/// hand).
fn in_order<K: Timed>(mut keys: Vec<K>) -> Vec<K> {
    if keys.windows(2).any(|pair| pair[0].tick() >= pair[1].tick()) {
        keys.sort_by_key(Timed::tick);
        keys.dedup_by_key(|key| key.tick());
    }
    keys
}
