//! Predictive local cursor.
//!
//! Video comes back ~100–200 ms after input (and at 30 fps with cheap capture
//! cards), so the guest's cursor lags. We draw our own cursor at the predicted
//! position immediately, find the guest's real cursor in each new frame, and
//! fold the difference back in. Comparisons are made against where our cursor
//! was one latency ago, so the correction doesn't fight the lag.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::crop::Crop;

/// Pixels whose green channel changed more than this count as changed.
const DIFF_THRESHOLD: u8 = 40;
/// Changed areas larger than this (either way) aren't the cursor.
const MAX_CURSOR: u32 = 64;
const CELL: usize = 4;
const HISTORY: Duration = Duration::from_millis(2500);
/// Keep showing the fake cursor this long after input stops, beyond the lag.
const LINGER: Duration = Duration::from_millis(150);
/// How long good lag/gain fits count towards the model.
const FIT_WINDOW: Duration = Duration::from_secs(60);
/// Share of the guest cursor's motion a fit must explain to be kept.
const MIN_FIT_R2: f64 = 0.8;
/// Furthest a tracked cursor can be from where we expect it.
const GATE: f64 = 400.0;
/// No detection for this long while moving counts as lost.
const LOST_AFTER: Duration = Duration::from_millis(1500);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blob {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
    pub pixels: u32,
}

impl Blob {
    fn w(&self) -> u32 {
        self.x1 - self.x0 + 1
    }
    fn h(&self) -> u32 {
        self.y1 - self.y0 + 1
    }
    fn top_left(&self) -> (f64, f64) {
        (self.x0 as f64, self.y0 as f64)
    }
}

/// Finds what changed between consecutive frames, grouped into blobs.
/// Owned by the receive thread.
#[derive(Default)]
pub struct ChangeFinder {
    prev: Vec<u8>,
    size: (u32, u32),
    cells: Vec<(u16, u16, u16, u16, u16)>, // per cell: x0, y0, x1, y1, count
}

impl ChangeFinder {
    /// Compare a BGRA frame with the previous one inside `region`. Returns
    /// None for the first frame or a frame identical to the last (sources repeat
    /// frames when the capture card is slower than the canvas).
    pub fn changes(&mut self, data: &[u8], stride: usize, w: u32, h: u32, region: Crop) -> Option<Vec<Blob>> {
        if self.size != (w, h) {
            self.size = (w, h);
            self.prev = vec![0; (w * h) as usize];
            for y in 0..h as usize {
                for x in 0..w as usize {
                    self.prev[y * w as usize + x] = data[y * stride + x * 4 + 1];
                }
            }
            return None;
        }
        let (cw, ch) = ((w as usize).div_ceil(CELL), (h as usize).div_ceil(CELL));
        self.cells.clear();
        self.cells.resize(cw * ch, (u16::MAX, u16::MAX, 0, 0, 0));
        let mut any = false;
        let (rx0, ry0) = (region.x as usize, region.y as usize);
        let (rx1, ry1) = ((region.x + region.w) as usize, (region.y + region.h) as usize);
        for y in ry0..ry1.min(h as usize) {
            let row = &data[y * stride..];
            let prev = &mut self.prev[y * w as usize..];
            for x in rx0..rx1.min(w as usize) {
                let g = row[x * 4 + 1];
                if g.abs_diff(prev[x]) > DIFF_THRESHOLD {
                    any = true;
                    let c = &mut self.cells[(y / CELL) * cw + x / CELL];
                    c.0 = c.0.min(x as u16);
                    c.1 = c.1.min(y as u16);
                    c.2 = c.2.max(x as u16);
                    c.3 = c.3.max(y as u16);
                    c.4 += 1;
                }
                prev[x] = g;
            }
        }
        if !any {
            return None;
        }
        // Group touching cells (8-connected).
        let mut blobs = Vec::new();
        let mut seen = vec![false; cw * ch];
        let mut stack = Vec::new();
        for start in 0..cw * ch {
            if seen[start] || self.cells[start].4 == 0 {
                continue;
            }
            let mut b = Blob { x0: u32::MAX, y0: u32::MAX, x1: 0, y1: 0, pixels: 0 };
            stack.push(start);
            seen[start] = true;
            while let Some(i) = stack.pop() {
                let c = self.cells[i];
                b.x0 = b.x0.min(c.0 as u32);
                b.y0 = b.y0.min(c.1 as u32);
                b.x1 = b.x1.max(c.2 as u32);
                b.y1 = b.y1.max(c.3 as u32);
                b.pixels += c.4 as u32;
                let (cx, cy) = ((i % cw) as isize, (i / cw) as isize);
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let (nx, ny) = (cx + dx, cy + dy);
                        if nx < 0 || ny < 0 || nx >= cw as isize || ny >= ch as isize {
                            continue;
                        }
                        let n = ny as usize * cw + nx as usize;
                        if !seen[n] && self.cells[n].4 > 0 {
                            seen[n] = true;
                            stack.push(n);
                        }
                    }
                }
            }
            blobs.push(b);
        }
        Some(blobs)
    }
}

/// Where the guest cursor's hotspot is now, given the changed areas of a new
/// frame, where it was (`prev`), where we expect it (`expected`) and its
/// sprite size (`size`, None until learned). The hotspot is taken as the
/// sprite's top-left, which is right for the usual arrow pointer.
///
/// When the cursor moves, the frame shows two changes: where it was erased
/// (overlapping its previous rectangle) and where it was drawn (nearby).
/// When it moved less than its own size, the two merge into one.
pub fn locate(
    blobs: &[Blob],
    prev: Option<(f64, f64)>,
    expected: (f64, f64),
    size: Option<(f64, f64)>,
) -> Option<Located> {
    let small: Vec<&Blob> = blobs
        .iter()
        .filter(|b| b.w() <= MAX_CURSOR && b.h() <= MAX_CURSOR && b.pixels >= 6)
        .collect();
    if small.is_empty() {
        return None;
    }
    let sprite = size.unwrap_or((16.0, 24.0));
    // The erased old cursor: the blob overlapping its previous rectangle most.
    let old = prev.and_then(|p| {
        small
            .iter()
            .enumerate()
            .map(|(i, b)| (i, overlap(b, p, sprite)))
            .filter(|(_, o)| *o > 0.0)
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    });
    let gap = |a: &Blob, p: (f64, f64)| {
        let c = (a.x0 as f64 + a.w() as f64 / 2.0, a.y0 as f64 + a.h() as f64 / 2.0);
        (c.0 - p.0).hypot(c.1 - p.1)
    };
    match (old, prev) {
        (Some(i), Some(p)) => {
            let o = small[i];
            let centre = (o.x0 as f64 + o.w() as f64 / 2.0, o.y0 as f64 + o.h() as f64 / 2.0);
            // Drawn somewhere else: the closest other change to the old spot.
            let new = small
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .map(|(_, b)| b)
                .filter(|b| gap(b, centre) < GATE)
                .min_by(|a, b| gap(a, centre).total_cmp(&gap(b, centre)));
            if let Some(n) = new {
                return Some(Located { at: n.top_left(), size: Some((o.w() as f64, o.h() as f64)) });
            }
            // Old and new overlap: the leading edge in each direction is the
            // new position. Needs the real sprite size.
            let (sw, sh) = size?;
            let pick = |lo: u32, hi: u32, e: f64, len: f64| {
                if (lo as f64) < e - 1.0 {
                    lo as f64
                } else {
                    (hi as f64 + 1.0 - len).max(lo as f64)
                }
            };
            Some(Located { at: (pick(o.x0, o.x1, p.0, sw), pick(o.y0, o.y1, p.1, sh)), size: None })
        }
        _ => {
            // Lost (or first sight): take the change closest to where we
            // expect the cursor.
            let b = small.iter().min_by(|a, b| gap(a, expected).total_cmp(&gap(b, expected)))?;
            Some(Located { at: b.top_left(), size: None })
        }
    }
}

/// Overlap area between a blob and the sprite rectangle at `p`.
fn overlap(b: &Blob, p: (f64, f64), size: (f64, f64)) -> f64 {
    let w = (b.x1 as f64 + 1.0).min(p.0 + size.0) - (b.x0 as f64).max(p.0);
    let h = (b.y1 as f64 + 1.0).min(p.1 + size.1) - (b.y0 as f64).max(p.1);
    if w > 0.0 && h > 0.0 { w * h } else { 0.0 }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Located {
    /// New hotspot position.
    pub at: (f64, f64),
    /// Sprite size, when this frame showed it cleanly.
    pub size: Option<(f64, f64)>,
}

/// Things that change on screen while the mouse is idle (blinking text
/// cursors, clocks) are remembered and ignored for a while.
#[derive(Default)]
struct FlickerFilter {
    boxes: Vec<(Blob, Instant)>,
}

const FLICKER_TTL: Duration = Duration::from_secs(20);

impl FlickerFilter {
    fn learn(&mut self, blobs: &[Blob], now: Instant) {
        self.boxes.retain(|(_, t)| now.duration_since(*t) < FLICKER_TTL);
        for b in blobs {
            if let Some(e) = self.boxes.iter_mut().find(|(k, _)| same_place(k, b)) {
                e.1 = now;
            } else {
                self.boxes.push((*b, now));
            }
        }
    }

    fn filter(&self, blobs: Vec<Blob>) -> Vec<Blob> {
        blobs
            .into_iter()
            .filter(|b| !self.boxes.iter().any(|(k, _)| same_place(k, b)))
            .collect()
    }
}

fn same_place(a: &Blob, b: &Blob) -> bool {
    const SLACK: u32 = 4;
    a.x0.abs_diff(b.x0) <= SLACK
        && a.y0.abs_diff(b.y0) <= SLACK
        && a.x1.abs_diff(b.x1) <= SLACK
        && a.y1.abs_diff(b.y1) <= SLACK
}

/// Per-frame cursor detection: changed areas → (minus known flicker) → the
/// guest cursor's new position, fed into the tracker. Owned by whichever
/// thread receives video.
#[derive(Default)]
pub struct CursorDetector {
    finder: ChangeFinder,
    flicker: FlickerFilter,
}

impl CursorDetector {
    /// Returns true if the frame differed from the previous one.
    pub fn process(
        &mut self,
        data: &[u8],
        stride: usize,
        w: u32,
        h: u32,
        tracker: &std::sync::Mutex<CursorTracker>,
        now: Instant,
    ) -> bool {
        let (bounds, prev, expected, size, idle, enabled) = {
            let t = tracker.lock().unwrap();
            (t.bounds(), t.guest(), t.expected_in_frame(now), t.size(), t.idle(now), t.enabled)
        };
        let Some(blobs) = self.finder.changes(data, stride, w, h, bounds) else {
            return false;
        };
        if !enabled {
            return true;
        }
        if idle {
            // The cursor can't move without input: whatever changed is
            // something else on screen.
            self.flicker.learn(&blobs, now);
            return true;
        }
        let blobs = self.flicker.filter(blobs);
        let found = locate(&blobs, prev, expected, size);
        if std::env::var_os("KVMS_CURSOR_DEBUG").is_some() {
            let desc: Vec<String> = blobs
                .iter()
                .take(5)
                .map(|b| format!("({},{})-({},{})#{}", b.x0, b.y0, b.x1, b.y1, b.pixels))
                .collect();
            eprintln!(
                "cursor: prev {:?} exp ({:.0},{:.0}) blobs {} -> {:?}",
                prev.map(|p| (p.0 as i32, p.1 as i32)),
                expected.0,
                expected.1,
                desc.join(" "),
                found.map(|f| (f.at.0 as i32, f.at.1 as i32))
            );
        }
        match found {
            Some(found) => tracker.lock().unwrap().detected(found, now),
            None => tracker.lock().unwrap().missed(),
        }
        true
    }
}

/// Drawn colour of our cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerState {
    /// Lined up with the guest's cursor.
    Synced,
    /// Ahead of the guest's cursor, which is being tracked.
    Predicting,
    /// Guest cursor not found: our position is a guess.
    Lost,
}

/// Within this many source pixels, ours and the guest's count as lined up.
const SYNCED_PX: f64 = 6.0;

#[derive(Debug, Clone, Copy, Default)]
pub struct TrackerStats {
    pub locked: bool,
    pub lag_ms: f64,
    pub gain: f64,
    pub detections_per_sec: f64,
    pub error_px: f64,
    /// Good fits behind the current lag/gain (last minute).
    pub fit_samples: u32,
}

pub struct CursorTracker {
    pub enabled: bool,
    captured: bool,
    bounds: Crop,
    /// Predicted guest hotspot now, in source pixels.
    local: Option<(f64, f64)>,
    /// Our predicted positions over time (shifted along with corrections).
    history: VecDeque<(Instant, (f64, f64))>,
    /// Uncorrected cumulative input, for fitting lag and gain.
    raw: VecDeque<(Instant, (f64, f64))>,
    raw_now: (f64, f64),
    detections: VecDeque<(Instant, (f64, f64))>,
    guest: Option<(f64, f64)>,
    last_input: Option<Instant>,
    last_fit: Option<Instant>,
    /// Good (time, lag ms, gain) fits from the last minute.
    fits: VecDeque<(Instant, f64, f64)>,
    lag: Duration,
    gain: f64,
    size: Option<(f64, f64)>,
    error: f64,
    misses: u32,
}

impl Default for CursorTracker {
    fn default() -> Self {
        Self {
            enabled: true,
            captured: false,
            bounds: Crop::full(1, 1),
            local: None,
            history: VecDeque::new(),
            raw: VecDeque::new(),
            raw_now: (0.0, 0.0),
            detections: VecDeque::new(),
            guest: None,
            last_input: None,
            last_fit: None,
            fits: VecDeque::new(),
            lag: Duration::from_millis(150),
            gain: 1.0,
            size: None,
            error: 0.0,
            misses: 0,
        }
    }
}

impl CursorTracker {
    pub fn set_bounds(&mut self, bounds: Crop) {
        self.bounds = bounds;
    }

    pub fn bounds(&self) -> Crop {
        self.bounds
    }

    pub fn set_captured(&mut self, captured: bool) {
        self.captured = captured;
    }

    pub fn guest(&self) -> Option<(f64, f64)> {
        self.guest
    }

    pub fn set_model(&mut self, lag_ms: f64, gain: f64) {
        if (10.0..1000.0).contains(&lag_ms) && (0.2..10.0).contains(&gain) {
            self.lag = Duration::from_secs_f64(lag_ms / 1000.0);
            self.gain = gain;
        }
    }

    /// The guest cursor's sprite size, once seen cleanly.
    pub fn size(&self) -> Option<(f64, f64)> {
        self.size
    }

    pub fn input(&mut self, dx: f64, dy: f64, now: Instant) {
        self.raw_now.0 += dx;
        self.raw_now.1 += dy;
        push(&mut self.raw, now, self.raw_now);
        let b = self.bounds;
        let start = self
            .local
            .or(self.guest)
            .unwrap_or((b.x as f64 + b.w as f64 / 2.0, b.y as f64 + b.h as f64 / 2.0));
        let p = (
            (start.0 + dx * self.gain).clamp(b.x as f64, (b.x + b.w) as f64 - 1.0),
            (start.1 + dy * self.gain).clamp(b.y as f64, (b.y + b.h) as f64 - 1.0),
        );
        self.local = Some(p);
        push(&mut self.history, now, p);
        self.last_input = Some(now);
    }

    /// Where we predicted the guest cursor would be at `t`.
    pub fn predicted_at(&self, t: Instant) -> Option<(f64, f64)> {
        at(&self.history, t).or(self.local)
    }

    pub fn expected_in_frame(&self, arrival: Instant) -> (f64, f64) {
        arrival
            .checked_sub(self.lag)
            .and_then(|t| self.predicted_at(t))
            .or(self.guest)
            .unwrap_or((
                self.bounds.x as f64 + self.bounds.w as f64 / 2.0,
                self.bounds.y as f64 + self.bounds.h as f64 / 2.0,
            ))
    }

    /// The guest cursor was seen at `g` in a frame that arrived at `arrival`.
    /// No input recently enough to have moved the cursor in this frame.
    pub fn idle(&self, now: Instant) -> bool {
        self.last_input
            .is_none_or(|t| now.duration_since(t) > self.lag + LINGER + Duration::from_millis(100))
    }

    /// True when we've had input but no detection for a while.
    pub fn lost(&self, now: Instant) -> bool {
        match (self.detections.back(), self.last_input) {
            (Some((t, _)), Some(_)) => now.duration_since(*t) > LOST_AFTER,
            (None, _) => true,
            _ => false,
        }
    }

    /// A changed frame during movement where the cursor wasn't found. A few
    /// in a row means we've lost it: forget the old position and re-acquire.
    pub fn missed(&mut self) {
        self.misses += 1;
        // Frames can change for other reasons while the cursor sits still,
        // so only give up after a run of misses.
        if self.misses >= 8 {
            self.guest = None;
        }
    }

    pub fn detected(&mut self, found: Located, arrival: Instant) {
        if let Some((w, h)) = found.size {
            self.size = Some(match self.size {
                Some(s) => (s.0 * 0.8 + w * 0.2, s.1 * 0.8 + h * 0.2),
                None => (w, h),
            });
        }
        let g = found.at;
        self.misses = 0;
        self.guest = Some(g);
        push(&mut self.detections, arrival, g);

        let moving = self.last_input.is_some_and(|t| arrival.duration_since(t) < self.lag + LINGER);
        match (moving, arrival.checked_sub(self.lag).and_then(|t| at(&self.history, t))) {
            (true, Some(then)) => {
                let err = (g.0 - then.0, g.1 - then.1);
                self.error = (err.0 * err.0 + err.1 * err.1).sqrt();
                // Move halfway each time: steady, and robust to one bad detection.
                let k = 0.5;
                let shift = (err.0 * k, err.1 * k);
                for (_, p) in self.history.iter_mut() {
                    p.0 += shift.0;
                    p.1 += shift.1;
                }
                if let Some(l) = self.local.as_mut() {
                    l.0 += shift.0;
                    l.1 += shift.1;
                }
            }
            _ => {
                // At rest: the guest has caught up, take its position.
                self.error = self.local.map_or(0.0, |l| ((g.0 - l.0).powi(2) + (g.1 - l.1).powi(2)).sqrt());
                self.local = Some(g);
                self.history.clear();
                push(&mut self.history, arrival, g);
            }
        }
        if self.last_fit.is_none_or(|t| arrival.duration_since(t) > Duration::from_millis(500)) {
            self.last_fit = Some(arrival);
            self.fit(arrival);
        }
    }

    /// Fit lag and gain from the last couple of seconds: for each candidate
    /// lag, regress guest positions on raw input (separate offsets per axis,
    /// shared slope) and keep the lag with the smallest residual. Good fits
    /// are kept for a minute; the model is their median, so one bad stretch
    /// doesn't move it but a real change takes over within about a minute.
    fn fit(&mut self, now: Instant) {
        if self.detections.len() < 10 {
            return;
        }
        let mut best: Option<(f64, Duration, f64, f64)> = None;
        for ms in (30..=400).step_by(10) {
            let lag = Duration::from_millis(ms);
            let pairs: Vec<((f64, f64), (f64, f64))> = self
                .detections
                .iter()
                .filter(|(_, g)| !self.at_edge(*g))
                .filter_map(|&(t, g)| Some((at(&self.raw, t.checked_sub(lag)?)?, g)))
                .collect();
            if pairs.len() < 10 {
                continue;
            }
            let n = pairs.len() as f64;
            let mean = |f: &dyn Fn(&((f64, f64), (f64, f64))) -> f64| pairs.iter().map(f).sum::<f64>() / n;
            let (rx, ry) = (mean(&|p| p.0 .0), mean(&|p| p.0 .1));
            let (gx, gy) = (mean(&|p| p.1 .0), mean(&|p| p.1 .1));
            let (mut num, mut den, mut var) = (0.0, 0.0, 0.0);
            for (r, g) in &pairs {
                num += (r.0 - rx) * (g.0 - gx) + (r.1 - ry) * (g.1 - gy);
                den += (r.0 - rx).powi(2) + (r.1 - ry).powi(2);
                var += (g.0 - gx).powi(2) + (g.1 - gy).powi(2);
            }
            // Needs real movement to say anything.
            if den < n * 400.0 {
                return;
            }
            let slope = num / den;
            let resid: f64 = pairs
                .iter()
                .map(|(r, g)| {
                    (g.0 - gx - slope * (r.0 - rx)).powi(2) + (g.1 - gy - slope * (r.1 - ry)).powi(2)
                })
                .sum::<f64>()
                / n;
            if best.is_none_or(|b| resid < b.0) {
                best = Some((resid, lag, slope, var / n));
            }
        }
        let Some((resid, lag, slope, var)) = best else { return };
        // Keep only fits where input explains the guest's motion well.
        let r2 = 1.0 - resid / var.max(1e-9);
        if std::env::var_os("KVMS_CURSOR_DEBUG").is_some() {
            eprintln!("cursor fit: lag {:?} gain {slope:.2} r2 {r2:.3}", lag);
        }
        if !(0.2..6.0).contains(&slope) || r2 < MIN_FIT_R2 {
            return;
        }
        self.fits.push_back((now, lag.as_secs_f64() * 1000.0, slope));
        while self.fits.front().is_some_and(|(t, ..)| now.duration_since(*t) > FIT_WINDOW) {
            self.fits.pop_front();
        }
        let lags: Vec<f64> = self.fits.iter().map(|f| f.1).collect();
        let gains: Vec<f64> = self.fits.iter().map(|f| f.2).collect();
        self.lag = Duration::from_secs_f64(median(lags) / 1000.0);
        self.gain = median(gains);
    }

    /// The guest clamps its cursor at the screen edge while input keeps
    /// going, so edge positions say nothing about lag or gain.
    fn at_edge(&self, g: (f64, f64)) -> bool {
        let b = self.bounds;
        g.0 <= b.x as f64 + 3.0
            || g.1 <= b.y as f64 + 3.0
            || g.0 + self.size.map_or(16.0, |s| s.0) >= (b.x + b.w) as f64 - 3.0
            || g.1 + self.size.map_or(24.0, |s| s.1) >= (b.y + b.h) as f64 - 3.0
    }

    /// How our cursor relates to the guest's right now.
    pub fn pointer_state(&self, now: Instant) -> PointerState {
        match (self.guest, self.local) {
            (None, _) => PointerState::Lost,
            _ if self.lost(now) => PointerState::Lost,
            (Some(g), Some(l)) if (g.0 - l.0).hypot(g.1 - l.1) <= SYNCED_PX => PointerState::Synced,
            _ => PointerState::Predicting,
        }
    }

    /// Position to draw our cursor at, if it should be shown.
    pub fn draw_position(&self, now: Instant) -> Option<(f64, f64)> {
        if !self.enabled || !self.captured {
            return None;
        }
        let local = self.local?;
        let recent = self.last_input.is_some_and(|t| now.duration_since(t) < self.lag + LINGER);
        let apart = self
            .guest
            .is_some_and(|g| ((g.0 - local.0).powi(2) + (g.1 - local.1).powi(2)).sqrt() > 3.0);
        (recent || apart).then_some(local)
    }

    pub fn stats(&self, now: Instant) -> TrackerStats {
        let recent = self
            .detections
            .iter()
            .filter(|(t, _)| now.duration_since(*t) < Duration::from_secs(1))
            .count();
        TrackerStats {
            locked: self.guest.is_some(),
            lag_ms: self.lag.as_secs_f64() * 1000.0,
            gain: self.gain,
            detections_per_sec: recent as f64,
            error_px: self.error,
            fit_samples: self.fits.len() as u32,
        }
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { (v[n / 2 - 1] + v[n / 2]) / 2.0 }
}

fn push(q: &mut VecDeque<(Instant, (f64, f64))>, t: Instant, p: (f64, f64)) {
    q.push_back((t, p));
    while q.front().is_some_and(|(f, _)| t.duration_since(*f) > HISTORY) {
        q.pop_front();
    }
}

/// Value at time `t` (the last sample at or before it).
fn at(q: &VecDeque<(Instant, (f64, f64))>, t: Instant) -> Option<(f64, f64)> {
    let i = q.partition_point(|(s, _)| *s <= t);
    (i > 0).then(|| q[i - 1].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 200;
    const H: u32 = 100;

    fn frame_with_sprite(at: Option<(u32, u32)>) -> Vec<u8> {
        let mut d = vec![100u8; (W * H * 4) as usize];
        if let Some((x0, y0)) = at {
            for y in y0..y0 + 16 {
                for x in x0..x0 + 10 {
                    d[((y * W + x) * 4 + 1) as usize] = 255;
                }
            }
        }
        d
    }

    fn blobs(a: Option<(u32, u32)>, b: Option<(u32, u32)>) -> Vec<Blob> {
        let mut f = ChangeFinder::default();
        let region = Crop::full(W, H);
        assert!(f.changes(&frame_with_sprite(a), (W * 4) as usize, W, H, region).is_none());
        f.changes(&frame_with_sprite(b), (W * 4) as usize, W, H, region).unwrap_or_default()
    }

    #[test]
    fn identical_frames_report_nothing() {
        let mut f = ChangeFinder::default();
        let d = frame_with_sprite(Some((10, 10)));
        let r = Crop::full(W, H);
        assert!(f.changes(&d, (W * 4) as usize, W, H, r).is_none());
        assert!(f.changes(&d, (W * 4) as usize, W, H, r).is_none());
    }

    #[test]
    fn separate_move_finds_new_position() {
        let b = blobs(Some((20, 20)), Some((80, 40)));
        assert_eq!(b.len(), 2);
        let p = locate(&b, Some((20.0, 20.0)), (75.0, 38.0), None).unwrap();
        assert_eq!(p.at, (80.0, 40.0));
        assert_eq!(p.size, Some((10.0, 16.0)));
    }

    #[test]
    fn overlapping_move_uses_leading_edge() {
        // Moved right 4 and down 3: old and new sprites overlap.
        let b = blobs(Some((50, 50)), Some((54, 53)));
        assert_eq!(b.len(), 1);
        let p = locate(&b, Some((50.0, 50.0)), (54.0, 53.0), Some((10.0, 16.0))).unwrap();
        assert_eq!(p.at, (54.0, 53.0));
        // Moved left/up.
        let b = blobs(Some((50, 50)), Some((46, 47)));
        let p = locate(&b, Some((50.0, 50.0)), (46.0, 47.0), Some((10.0, 16.0))).unwrap();
        assert_eq!(p.at, (46.0, 47.0));
    }

    #[test]
    fn large_changes_are_ignored() {
        let mut f = ChangeFinder::default();
        let r = Crop::full(W, H);
        let a = vec![0u8; (W * H * 4) as usize];
        let b = vec![255u8; (W * H * 4) as usize];
        f.changes(&a, (W * 4) as usize, W, H, r);
        let blobs = f.changes(&b, (W * 4) as usize, W, H, r).unwrap();
        assert_eq!(locate(&blobs, None, (0.0, 0.0), None), None);
    }

    #[test]
    fn overlap_needs_learned_size() {
        let b = blobs(Some((50, 50)), Some((54, 53)));
        assert_eq!(locate(&b, Some((50.0, 50.0)), (54.0, 53.0), None), None);
    }

    #[test]
    fn model_ignores_a_bad_stretch() {
        let mut t = CursorTracker::default();
        let t0 = Instant::now();
        for (i, lag) in [180.0, 185.0, 190.0, 400.0, 182.0].iter().enumerate() {
            t.fits.push_back((t0 + Duration::from_secs(i as u64), *lag, 4.0));
        }
        let lags: Vec<f64> = t.fits.iter().map(|f| f.1).collect();
        assert_eq!(median(lags), 185.0);
    }

    #[test]
    fn idle_changes_are_learned_as_flicker() {
        let tracker = std::sync::Mutex::new(CursorTracker::default());
        tracker.lock().unwrap().set_bounds(Crop::full(W, H));
        let mut d = CursorDetector::default();
        let t0 = Instant::now();
        let s = (W * 4) as usize;
        // Frame 1: baseline. Frame 2: a "text cursor" appears while idle.
        d.process(&frame_with_sprite(None), s, W, H, &tracker, t0);
        d.process(&frame_with_sprite(Some((120, 40))), s, W, H, &tracker, t0);
        assert_eq!(tracker.lock().unwrap().guest(), None);
        // Now the user moves; the blink happens again and must not be taken
        // as the cursor.
        tracker.lock().unwrap().input(5.0, 0.0, t0 + Duration::from_millis(10));
        d.process(&frame_with_sprite(None), s, W, H, &tracker, t0 + Duration::from_millis(40));
        assert_eq!(tracker.lock().unwrap().guest(), None);
    }

    #[test]
    fn tracker_predicts_then_settles_on_guest() {
        let mut t = CursorTracker::default();
        t.set_bounds(Crop::full(1000, 1000));
        t.set_captured(true);
        let t0 = Instant::now();
        t.detected(Located { at: (100.0, 100.0), size: None }, t0);
        assert_eq!(t.draw_position(t0), None); // at rest, guest visible
        t.input(10.0, 0.0, t0 + Duration::from_millis(10));
        assert_eq!(t.draw_position(t0 + Duration::from_millis(10)), Some((110.0, 100.0)));
        // Long after, guest reports where it ended up: we adopt it and hide.
        let later = t0 + Duration::from_secs(1);
        t.detected(Located { at: (112.0, 100.0), size: None }, later);
        assert_eq!(t.draw_position(later), None);
        assert_eq!(t.predicted_at(later), Some((112.0, 100.0)));
    }

    #[test]
    fn fit_recovers_lag_and_gain() {
        let mut t = CursorTracker::default();
        t.set_bounds(Crop::full(4000, 4000));
        t.set_captured(true);
        let t0 = Instant::now();
        let true_lag = Duration::from_millis(120);
        let true_gain = 2.0;
        let mut raw = (0.0, 0.0);
        let mut inputs = Vec::new();
        // Zig-zag input at 250 Hz for 2 s.
        for i in 0..500u64 {
            let dx = if (i / 50) % 2 == 0 { 3.0 } else { -3.0 };
            let dy = if (i / 80) % 2 == 0 { 2.0 } else { -2.0 };
            let now = t0 + Duration::from_millis(i * 4);
            raw.0 += dx;
            raw.1 += dy;
            inputs.push((now, raw));
            t.input(dx, dy, now);
            // Guest frames at 30 fps showing where input put it one lag ago.
            if i % 8 == 0 {
                if let Some(seen) = now.checked_sub(true_lag) {
                    if let Some(&(_, r)) = inputs.iter().rev().find(|(ts, _)| *ts <= seen) {
                        t.detected(Located { at: (2000.0 + r.0 * true_gain, 2000.0 + r.1 * true_gain), size: None }, now);
                    }
                }
            }
        }
        let s = t.stats(t0 + Duration::from_secs(2));
        assert!(s.fit_samples > 0);
        assert!((s.lag_ms - 120.0).abs() < 15.0, "lag {}", s.lag_ms);
        assert!((s.gain - 2.0).abs() < 0.15, "gain {}", s.gain);
    }
}
