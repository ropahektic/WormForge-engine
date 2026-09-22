//! Fog-of-war visibility model: a Rust port of wkFOW's `Vision.cpp` and
//! `VisionTaskPool.h`, reduced to the core fog feature set.
//!
//! The load-bearing pieces are ported at full fidelity:
//!
//!   * symmetric recursive shadowcasting over the 8 octants, with the
//!     cone-slope-range clipping and the rule that occlusion is NOT gated on
//!     the visible shape (terrain blocks sight whether or not we light the
//!     cell it sits in);
//!   * the synchronous fork-join cast pool (8 octants claimed from a shared
//!     counter, caller also drains, join within the frame);
//!   * the two-tier LOD: native 1 cell/px out to `NATIVE_DETAIL_RADIUS`, then
//!     coarse `FAR_FIELD_CELL_SIZE` cells out to full reach, with the
//!     far-aperture wedge clip keeping shadow edges continuous across the
//!     tier boundary;
//!   * per-source cached masks unioned into a per-cell source COUNT, so a
//!     moved worm re-casts only its own mask;
//!   * the 2-cell feather ring (255 core / 192 inner / 96 outer);
//!   * live explosion reveals with the `full*(1-t^12)` radius easing.
//!
//! Nothing here knows about WA's memory layout; solidity arrives as a closure
//! over the landscape's 1bpp collision grid.

use crate::discovery::{Discovery, Mode};
use crate::settings::Scheme;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};

// ---------------------------------------------------------------------------
// Engine constants (Client-scope perf knobs in wkFOW; baked in here).
// ---------------------------------------------------------------------------

/// Where the cast drops from native resolution to the coarse far tier, in
/// landscape pixels. This is a resolution boundary, not a vision boundary:
/// `circle_radius`/`cone_length` decide how far a source sees, this decides
/// only how sharply that is computed.
pub const NATIVE_DETAIL_RADIUS: i32 = 250;

/// Coarse far-tier cell size, in landscape pixels per cell. A coarse cell
/// blocks sight if ANY collision pixel inside it blocks (conservative).
pub const FAR_FIELD_CELL_SIZE: i32 = 3;

/// Feather ring width in cells, and its two light levels. The feather is a
/// cosmetic ramp painted PAST the true vision boundary, so widening or
/// brightening it would read further around a corner than the rules allow.
pub const FEATHER_CELLS: i32 = 2;
pub const INNER_LIGHT: u8 = 192;
pub const OUTER_LIGHT: u8 = 96;

/// Overlap between the two LODs so quantisation cannot open a black ring
/// where the tiers meet. Both tiers are cast in the same frame, so this
/// covers quantisation only -- there is no temporal lag for it to hide.
const FAR_HANDOFF_OVERLAP_PIXELS: i32 = 32;

/// Rim directions sampled to derive a near mask's far aperture. An even count
/// keeps every wedge edge off the horizontal (see `FarWedge`).
const APERTURE_DIRECTIONS: usize = 2048;
/// Probe just inside the rim: the outermost stamped ring is partial and would
/// otherwise read as blocked all the way round.
const APERTURE_PROBE_MARGIN_PIXELS: i32 = 3;
/// The row solver intersects two half planes, which only describes a wedge
/// narrower than a half turn. A quarter turn stays comfortably inside that.
const APERTURE_MAX_WEDGE_BINS: usize = APERTURE_DIRECTIONS / 4;
/// Bounds the cost of a pathological many-worm, heavily broken-terrain frame.
const MAX_PUBLISHED_APERTURE_WEDGES: usize = 512;

/// Map dimension sanity bounds. Long roping maps legitimately reach almost
/// 30,000 pixels wide, so an individual-axis limit alone must not reject
/// them; retain a total-cell ceiling as the allocation sanity guard.
pub const MAX_MAP_DIM: usize = 32768;
pub const MAX_MAP_PIXELS: usize = 96 * 1024 * 1024;

fn map_dimensions_sane(width: usize, height: usize) -> bool {
    width > 0 && height > 0 && width <= MAX_MAP_DIM && height <= MAX_MAP_DIM
        && width * height <= MAX_MAP_PIXELS
}

/// How long a blast stays lit, and the sharpness of its close. The radius is
/// `full * (1 - t^N)` with t running 0..1 across the window: the circle holds
/// wide while the smoke is thick, then closes decisively.
pub const EXPLOSION_FADE_MS: u64 = 2000;
const EXPLOSION_FADE_POWER: u32 = 12;
/// An airstrike is a dozen craters in under a second and every one of them is
/// a cast. Past this the oldest are dropped.
const EXPLOSION_MAX_LIVE: usize = 24;

/// A falling worm can disappear from the source list for a handful of frames
/// before it is inserted again. Keep its last mask through that gap.
const EMPTY_SOURCE_GRACE_FRAMES: u32 = 8;

/// Grow the light canvas in 256px blocks rather than reallocating every time
/// a flying/roping worm crosses a single pixel.
const CHUNK_CELLS: i32 = 256;

const SHADOW_OCTANT_COUNT: usize = 8;

// ---------------------------------------------------------------------------
// VisionSource / SourceKind
// ---------------------------------------------------------------------------

/// What kind of thing is doing the seeing. Each kind resolves to circle, cone
/// and LOS geometry; crates bake the latter two instead of exposing settings.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum SourceKind {
    #[default]
    Worm = 0,
    Animal = 1,
    Projectile = 2,
    Crate = 3,
    Explosion = 4,
}

const SOURCE_KIND_COUNT: usize = 5;

impl SourceKind {
    fn index(self) -> usize {
        self as usize
    }

    /// May this kind of source write terrain into permanent memory?
    ///
    /// Worms and animals: yes -- both are ownership-gated upstream.
    /// Projectiles, crates, explosions: no. Their light is public but
    /// transient; recording it would leave a permanent survey of wherever a
    /// shell happened to fly. Explosions record their CRATER through
    /// `Discovery::notify_crater` instead; the circle here stays pure
    /// temporary light.
    fn discovers(self) -> bool {
        !matches!(
            self,
            SourceKind::Projectile | SourceKind::Crate | SourceKind::Explosion
        )
    }
}

/// One frame's view of a single vision source. The caller builds these
/// already ownership-filtered; `Fog::update` matches them to last frame's
/// cached masks by `same_source_key`.
#[derive(Clone, Debug, Default)]
pub struct VisionSource {
    pub kind: SourceKind,
    /// Identity, so this frame's source can be matched to last frame's mask.
    /// Worms are (team, worm); everything else uses `id` and leaves the worm
    /// pair at zero.
    pub team_number: i32,
    pub worm_number: i32,
    pub id: u32,
    /// Position in landscape pixels.
    pub x: i32,
    pub y: i32,
    pub aim_angle: i32,
    pub dir_x: i32,
    pub dir_y: i32,
    pub facing: i32,
    pub active: bool,
    /// -1 uses the kind-wide radius. Projectiles override this with their
    /// live sprite bound plus `proj_circle` padding.
    pub circle_radius: i32,
    /// Does this source's weapon have an aim sweep to follow? False for
    /// droppables, whose dirX/dirY hold a stale angle; such a source falls
    /// back to a flat cone along its facing. Always true for non-worm kinds.
    pub aimable: bool,
}

impl VisionSource {
    /// Convenience constructor: a worm source at (x, y) with defaults
    /// (facing right, inactive, aimable, kind-wide radius).
    pub fn worm(x: i32, y: i32) -> Self {
        VisionSource {
            kind: SourceKind::Worm,
            x,
            y,
            facing: 1,
            aimable: true,
            circle_radius: -1,
            ..Default::default()
        }
    }
}

fn same_source_key(a: &VisionSource, b: &VisionSource) -> bool {
    a.kind == b.kind
        && a.team_number == b.team_number
        && a.worm_number == b.worm_number
        && a.id == b.id
}

/// Geometry only. `active` must be included because idle cones use sprite
/// facing and the active worm's cone uses its aim direction.
fn same_source_geometry(a: &VisionSource, b: &VisionSource) -> bool {
    same_source_key(a, b)
        && a.x == b.x
        && a.y == b.y
        && a.aim_angle == b.aim_angle
        && a.dir_x == b.dir_x
        && a.dir_y == b.dir_y
        && a.facing == b.facing
        && a.active == b.active
        && a.circle_radius == b.circle_radius
}

/// In hybrid mode the native mask contains only the radius circle. Rotating
/// the aim must not make that collision-tested circle expensive to rebuild.
fn same_near_source_geometry(a: &VisionSource, b: &VisionSource) -> bool {
    same_source_key(a, b) && a.x == b.x && a.y == b.y
        && a.circle_radius == b.circle_radius
}

// ---------------------------------------------------------------------------
// SourceGeometry
// ---------------------------------------------------------------------------

/// The four numbers that decide what one source can see, plus the two values
/// derived from them, resolved once per frame for each kind.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
struct SourceGeometry {
    circle_radius: i32,
    cone_width: i32,
    cone_length: i32,
    los: bool,
    /// Where the cast drops to the coarse cell size, and whether that coarse
    /// tier runs at all.
    native_detail_radius: i32,
    hybrid_far_active: bool,
}

impl SourceGeometry {
    /// How far this kind can see in any direction.
    fn extent(&self) -> i32 {
        self.circle_radius.max(self.cone_length).max(0)
    }

    /// A kind with neither a circle nor a cone is not a vision source.
    fn sees(&self) -> bool {
        self.circle_radius > 0 || self.cone_length > 0
    }
}

// ---------------------------------------------------------------------------
// ShadowVisionShape: circle + aim cone with the integer cross-product test.
// ---------------------------------------------------------------------------

struct ShadowVisionShape {
    circle_radius: i32,
    cone_length: i32,
    cone_width: i32,
    cone_dir_x: i32,
    cone_dir_y: i32,
    // The cone edge directions are calculated once per source. contains()
    // then needs only two integer cross products per distant cell.
    cone_left_x: i32,
    cone_left_y: i32,
    cone_right_x: i32,
    cone_right_y: i32,
    max_radius: i32,
    circle_radius_sq: i64,
    cone_length_sq: i64,
    cone_direction_length_sq: i64,
    cone_cos_squared: f64,
}

impl ShadowVisionShape {
    fn direction_in_cone(&self, dx: f64, dy: f64) -> bool {
        if self.cone_length <= 0 || self.cone_width <= 0 {
            return false;
        }
        if self.cone_width >= 360 {
            return true;
        }
        let distance_sq = dx * dx + dy * dy;
        if distance_sq == 0.0 {
            return true;
        }
        let dot = dx * self.cone_dir_x as f64 + dy * self.cone_dir_y as f64;
        let threshold =
            distance_sq * self.cone_direction_length_sq as f64 * self.cone_cos_squared;
        let dot_sq = dot * dot;
        if self.cone_width <= 180 {
            return dot >= 0.0 && dot_sq >= threshold;
        }
        // For an obtuse cone every forward point is included. A rear point is
        // included only if it lies inside the wide angle's back-facing span.
        dot >= 0.0 || dot_sq <= threshold
    }

    fn contains(&self, dx: i32, dy: i32) -> bool {
        let distance_sq = dx as i64 * dx as i64 + dy as i64 * dy as i64;
        if self.circle_radius > 0 && distance_sq <= self.circle_radius_sq {
            return true;
        }
        if self.cone_length <= 0 || self.cone_width <= 0 || distance_sq > self.cone_length_sq
        {
            return false;
        }
        if self.cone_width >= 360 {
            return true;
        }
        let left_cross =
            self.cone_left_x as i64 * dy as i64 - self.cone_left_y as i64 * dx as i64;
        let right_cross =
            dx as i64 * self.cone_right_y as i64 - dy as i64 * self.cone_right_x as i64;
        if self.cone_width <= 180 {
            left_cross >= 0 && right_cross >= 0
        } else {
            left_cross >= 0 || right_cross >= 0
        }
    }
}

/// Build the ShadowVisionShape for one source: circle, cone reach, cone
/// width, and the direction the cone points. Shared by the shadowcaster and
/// the no-collision fill below so the two cannot disagree about the shape
/// they are producing.
fn build_vision_shape(
    source: &VisionSource,
    circle_radius: i32,
    cone_width: i32,
    cone_length: i32,
) -> ShadowVisionShape {
    let mut shape = ShadowVisionShape {
        circle_radius: circle_radius.max(0),
        cone_length: cone_length.max(0),
        cone_width: cone_width.clamp(0, 360),
        cone_dir_x: source.dir_x,
        cone_dir_y: source.dir_y,
        cone_left_x: 1024,
        cone_left_y: 0,
        cone_right_x: 1024,
        cone_right_y: 0,
        max_radius: 0,
        circle_radius_sq: 0,
        cone_length_sq: 0,
        cone_direction_length_sq: 0,
        cone_cos_squared: 0.0,
    };
    shape.max_radius = shape.circle_radius.max(shape.cone_length);
    shape.circle_radius_sq = shape.circle_radius as i64 * shape.circle_radius as i64;
    shape.cone_length_sq = shape.cone_length as i64 * shape.cone_length as i64;

    // Flat along the facing when there is no live aim to follow: either the
    // worm is idle, or its weapon has no aim sweep at all. Both leave
    // dirX/dirY holding a stale angle.
    if !source.active || !source.aimable {
        shape.cone_dir_x = if source.facing < 0 { -1024 } else { 1024 };
        shape.cone_dir_y = 0;
    }
    if shape.cone_dir_x == 0 && shape.cone_dir_y == 0 {
        shape.cone_dir_x = 1024;
    }
    shape.cone_direction_length_sq = shape.cone_dir_x as i64 * shape.cone_dir_x as i64
        + shape.cone_dir_y as i64 * shape.cone_dir_y as i64;
    let half_angle = shape.cone_width as f64 * std::f64::consts::PI / 360.0;
    let cone_centre = (shape.cone_dir_y as f64).atan2(shape.cone_dir_x as f64);
    shape.cone_left_x = ((cone_centre - half_angle).cos() * 1024.0) as i32;
    shape.cone_left_y = ((cone_centre - half_angle).sin() * 1024.0) as i32;
    shape.cone_right_x = ((cone_centre + half_angle).cos() * 1024.0) as i32;
    shape.cone_right_y = ((cone_centre + half_angle).sin() * 1024.0) as i32;
    let cone_cos = half_angle.cos();
    shape.cone_cos_squared = cone_cos * cone_cos;
    shape
}

// ---------------------------------------------------------------------------
// Far aperture (the wedge clip that keeps shadow edges continuous across the
// tier boundary).
// ---------------------------------------------------------------------------

/// One wedge of admitted far-field directions. Each edge bounds the column
/// offset from the apex linearly in the row offset, so testing a row costs
/// two multiplies and no trigonometry.
#[derive(Clone, Copy, Debug, Default)]
pub struct FarWedge {
    pub start_cotangent: f32,
    pub end_cotangent: f32,
    pub start_is_upper_bound: bool,
    pub end_is_lower_bound: bool,
}

/// The directions a source can still see at its near mask's rim. The far
/// field is admitted only inside these wedges, so a near occluder's shadow
/// continues outward along the very ray the near mask ended on.
#[derive(Clone, Debug, Default)]
pub struct FarAperture {
    /// Wedge apex: the source position this near mask was built from.
    pub x: i32,
    pub y: i32,
    /// false means this source obstructs nothing, so the union over sources
    /// admits every direction and the clip can be skipped entirely.
    pub restricted: bool,
    pub wedges: Vec<FarWedge>,
}

// ---------------------------------------------------------------------------
// SourceMask: the hard-lit cells contributed by one source.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Default)]
struct SourceCell {
    local_x: u16,
    local_y: u16,
}

struct SourceMask {
    source: VisionSource,
    /// Origin and lit bounds are in this mask's cell coordinates. Native
    /// masks use one landscape pixel per cell; far masks use FAR_FIELD_CELL_SIZE.
    cell_size: i32,
    origin_x: i32,
    origin_y: i32,
    width: i32,
    height: i32,
    /// Tight world-space bounds of cells actually lit by this source. The
    /// allocation remains a square for fast ray deduplication, but feathering
    /// must not scan that whole potential square for a narrow long cone.
    lit_left: i32,
    lit_top: i32,
    lit_right: i32,
    lit_bottom: i32,
    cells: Vec<SourceCell>,
    cells_sorted: bool,
    /// Directions this source can still see at its rim, for the far clip.
    /// Only near masks fill these.
    aperture: FarAperture,
    aperture_valid: bool,
}

impl SourceMask {
    fn new() -> Self {
        SourceMask {
            source: VisionSource::default(),
            cell_size: 1,
            origin_x: 0,
            origin_y: 0,
            width: 0,
            height: 0,
            lit_left: 0,
            lit_top: 0,
            lit_right: 0,
            lit_bottom: 0,
            cells: Vec::new(),
            cells_sorted: false,
            aperture: FarAperture::default(),
            aperture_valid: false,
        }
    }
}

// ---------------------------------------------------------------------------
// CastAccumulator: per-thread cast output. mark_lit cannot touch the shared
// mask's cell list or bounds while octants run in parallel, so each thread
// folds its own copy and they are merged at the join. Only the stamp count is
// kept, not the cells themselves: the cell list is rebuilt from the bitmap in
// row order afterwards.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct CastAccumulator {
    lit_left: i32,
    lit_top: i32,
    lit_right: i32,
    lit_bottom: i32,
    any: bool,
    stamped: usize,
}

impl CastAccumulator {
    #[inline]
    fn note(&mut self, cell_x: i32, cell_y: i32) {
        if !self.any {
            self.lit_left = cell_x;
            self.lit_top = cell_y;
            self.lit_right = cell_x + 1;
            self.lit_bottom = cell_y + 1;
            self.any = true;
        } else {
            if cell_x < self.lit_left {
                self.lit_left = cell_x;
            }
            if cell_y < self.lit_top {
                self.lit_top = cell_y;
            }
            if cell_x + 1 > self.lit_right {
                self.lit_right = cell_x + 1;
            }
            if cell_y + 1 > self.lit_bottom {
                self.lit_bottom = cell_y + 1;
            }
        }
        self.stamped += 1;
    }
}

// ---------------------------------------------------------------------------
// The shared cast context. One cast at a time runs inside `Fog::update`
// (which is single-threaded), so a single set of scratch state is enough.
// The octants of a single cast only ever *set* stamps to the same generation
// value, so the bitmap is a monotonic set union: octant order cannot change
// the result. Parallel octants therefore share it, stamping through atomics.
// ---------------------------------------------------------------------------

struct CastScratch {
    stamps: Vec<AtomicU16>,
    generation: u16,
}

impl CastScratch {
    fn new() -> Self {
        CastScratch {
            stamps: Vec::new(),
            generation: 0,
        }
    }

    fn begin(&mut self, cell_count: usize) {
        if self.stamps.len() < cell_count {
            self.stamps.resize_with(cell_count, || AtomicU16::new(0));
        }
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            // Only the 16-bit generation wrap needs a full clear.
            for s in &self.stamps {
                s.store(0, Ordering::Relaxed);
            }
            self.generation = 1;
        }
    }

    /// Mark one cell lit. Returns true if this call was the first to stamp
    /// the cell this generation (i.e. the accumulator should count it).
    /// Relaxed is sufficient: the only value ever stored is the current
    /// generation, so a lost race just means two octants both count a shared
    /// boundary cell. That overcounts the reserve, never the emitted cells.
    #[inline]
    fn stamp(&self, index: usize) -> bool {
        let stamp = &self.stamps[index];
        if stamp.load(Ordering::Relaxed) == self.generation {
            return false;
        }
        stamp.store(self.generation, Ordering::Relaxed);
        true
    }

    #[inline]
    fn is_stamped(&self, index: usize) -> bool {
        self.stamps[index].load(Ordering::Relaxed) == self.generation
    }
}

// ---------------------------------------------------------------------------
// CastPool: synchronous fork/join for independent octants, a port of
// VisionTaskPool.h. Every cast finishes inside the frame that asked for it,
// so near and far light are always built from the same source position. That
// is what removes the tier seam -- an asynchronous worker can only ever
// deliver a position several frames stale.
//
// The pool owns a set of persistent worker threads. `run` publishes a task
// body under the mutex; workers and the caller claim items from a shared
// counter until the batch drains, then the caller blocks until every claimed
// item has completed. The only unsafe in this module is the lifetime erasure
// that publishes the caller's stack-bound closure to the workers: sound
// because run() joins before returning, exactly as the C++ does.
// ---------------------------------------------------------------------------

struct PoolShared {
    // The current batch. `body` is a type-erased pointer to the caller's
    // closure, valid only while `remaining > 0` or items are unclaimed; run()
    // waits for full completion before clearing it.
    body: Option<*const (dyn Fn(usize) + Sync)>,
    next: usize,
    count: usize,
    remaining: usize,
    stopping: bool,
}

struct CastPool {
    shared: Mutex<PoolShared>,
    wake: Condvar,
    finished: Condvar,
    workers: std::sync::Mutex<Vec<std::thread::JoinHandle<()>>>,
    started: AtomicUsize, // 0 = not started, 1 = started
}

// The raw pointer in PoolShared is only dereferenced by pool threads while
// run() is on the stack, and run() never returns until every claimed item has
// finished. That is the same contract VisionTaskPool.h documents.
unsafe impl Send for PoolShared {}
unsafe impl Sync for PoolShared {}

impl CastPool {
    fn new() -> Self {
        CastPool {
            shared: Mutex::new(PoolShared {
                body: None,
                next: 0,
                count: 0,
                remaining: 0,
                stopping: false,
            }),
            wake: Condvar::new(),
            finished: Condvar::new(),
            workers: std::sync::Mutex::new(Vec::new()),
            started: AtomicUsize::new(0),
        }
    }

    /// Worker count = min(hardware-1, 7), matching wkFOW's CastPool.
    fn ensure_started(&self) {
        if self.started.swap(1, Ordering::SeqCst) != 0 {
            return;
        }
        let hardware = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let hardware = if hardware == 0 || hardware > 256 {
            1
        } else {
            hardware
        };
        let workers = (hardware - 1).min(7);
        let mut handles = self.workers.lock().unwrap();
        for _ in 0..workers {
            let shared = &self.shared as *const Mutex<PoolShared> as usize;
            let wake = &self.wake as *const Condvar as usize;
            let finished = &self.finished as *const Condvar as usize;
            // The pool is owned by Fog and outlives every batch; workers are
            // joined in shutdown() before the pool is dropped.
            let handle = std::thread::spawn(move || unsafe {
                let shared = &*(shared as *const Mutex<PoolShared>);
                let wake = &*(wake as *const Condvar);
                let finished = &*(finished as *const Condvar);
                worker_main(shared, wake, finished);
            });
            handles.push(handle);
        }
    }

    /// Synchronous fork/join: claim items from the shared counter, caller
    /// also drains, join within the frame.
    fn run(&self, items: usize, task: &(dyn Fn(usize) + Sync)) {
        if items == 0 {
            return;
        }
        self.ensure_started();
        let mut lock = self.shared.lock().unwrap();
        lock.body = Some(unsafe {
            std::mem::transmute::<*const (dyn Fn(usize) + Sync), *const (dyn Fn(usize) + Sync)>(
                task,
            )
        });
        lock.count = items;
        lock.remaining = items;
        lock.next = 0;
        self.wake.notify_all();
        lock = drain(lock, &self.shared, &self.finished);
        while lock.remaining != 0 {
            lock = self.finished.wait(lock).unwrap();
        }
        lock.body = None;
    }

    fn shutdown(&self) {
        {
            let mut lock = self.shared.lock().unwrap();
            lock.stopping = true;
        }
        self.wake.notify_all();
        let mut handles = self.workers.lock().unwrap();
        for h in handles.drain(..) {
            let _ = h.join();
        }
        self.started.store(0, Ordering::SeqCst);
    }
}

impl Drop for CastPool {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Claim and run items until the batch drains, then return the lock. Claim
/// and completion share the publication mutex: a late worker cannot acquire
/// an old body after its last task has completed.
fn drain<'a>(
    mut lock: std::sync::MutexGuard<'a, PoolShared>,
    shared: &'a Mutex<PoolShared>,
    finished: &'a Condvar,
) -> std::sync::MutexGuard<'a, PoolShared> {
    while let (Some(body), true) = (lock.body, lock.next < lock.count) {
        let item = lock.next;
        lock.next += 1;
        drop(lock);
        // SAFETY: run() published `body` and is blocked waiting for
        // remaining == 0; it cannot return while this item is in flight, so
        // the closure outlives the call.
        unsafe { (*body)(item) };
        lock = shared.lock().unwrap();
        lock.remaining -= 1;
        if lock.remaining == 0 {
            finished.notify_one();
        }
    }
    lock
}

fn worker_main(shared: &Mutex<PoolShared>, wake: &Condvar, finished: &Condvar) {
    let mut lock = shared.lock().unwrap();
    loop {
        while !lock.stopping && !(lock.body.is_some() && lock.next < lock.count) {
            lock = wake.wait(lock).unwrap();
        }
        if lock.stopping {
            return;
        }
        lock = drain(lock, shared, finished);
    }
}

// ---------------------------------------------------------------------------
// Symmetric recursive shadowcasting.
// ---------------------------------------------------------------------------

const OCTANTS: [[i32; 4]; SHADOW_OCTANT_COUNT] = [
    [1, 0, 0, 1],
    [0, 1, 1, 0],
    [0, 1, -1, 0],
    [-1, 0, 0, 1],
    [-1, 0, 0, -1],
    [0, -1, -1, 0],
    [0, -1, 1, 0],
    [1, 0, 0, -1],
];

// The comparisons mirror Vision.cpp's firstShadowColumn exactly; the slopes
// are finite by construction, so the partial-ord lint does not apply.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
fn first_shadow_column(distance: i32, start_slope: f64) -> i32 {
    // A full octant cannot skip any columns; keep that common case cheap.
    if start_slope >= 1.0 {
        return -distance;
    }
    let denominator = -(distance as f64) - 0.5;
    // For the cast's nonnegative slopes this is negative, so truncating
    // toward zero supplies ceil without a floating-point library call.
    let mut dx = ((start_slope * denominator - 0.5) as i32).clamp(-distance, 1);
    // rightSlope <= startSlope is monotone as dx increases. Correct both ways
    // with the ORIGINAL division/comparison, so only columns the old loop
    // would continue past are skipped. dx == 1 means none.
    while dx > -distance && !(start_slope < ((dx - 1) as f64 + 0.5) / denominator) {
        dx -= 1;
    }
    while dx <= 0 && start_slope < (dx as f64 + 0.5) / denominator {
        dx += 1;
    }
    dx
}

/// Everything the octant cast needs that does not change during a cast.
/// Bundled so the recursive signature stays readable.
struct CastEnv<'a> {
    mask_origin_x: i32,
    mask_origin_y: i32,
    mask_width: i32,
    mask_height: i32,
    shape: &'a ShadowVisionShape,
    cell_size: i32,
    scratch: &'a CastScratch,
    solid: &'a (dyn Fn(i32, i32) -> bool + Sync),
    minimum_light_distance_sq: i64,
    minimum_opaque_distance_sq: i64,
}

impl CastEnv<'_> {
    #[inline]
    fn mark_lit(&self, acc: &mut CastAccumulator, cell_x: i32, cell_y: i32) {
        let local_x = cell_x - self.mask_origin_x;
        let local_y = cell_y - self.mask_origin_y;
        if local_x < 0
            || local_x >= self.mask_width
            || local_y < 0
            || local_y >= self.mask_height
        {
            return;
        }
        let index = (local_y as usize) * (self.mask_width as usize) + local_x as usize;
        if self.scratch.stamp(index) {
            acc.note(cell_x, cell_y);
        }
    }

    /// A coarse far cell blocks sight if any collision pixel inside it
    /// blocks. This is intentionally conservative: a lower-resolution cone
    /// may hide an extra pixel at terrain edges, but must never see through a
    /// thin wall.
    #[inline]
    fn solid_block(&self, x: i32, y: i32, size: i32) -> bool {
        if size <= 1 {
            return (self.solid)(x, y);
        }
        for offset_y in 0..size {
            for offset_x in 0..size {
                if (self.solid)(x + offset_x, y + offset_y) {
                    return true;
                }
            }
        }
        false
    }
}

#[allow(clippy::too_many_arguments)]
fn cast_shadow_octant(
    env: &CastEnv,
    acc: &mut CastAccumulator,
    source_cell_x: i32,
    source_cell_y: i32,
    row: i32,
    start_slope: f64,
    end_slope: f64,
    xx: i32,
    xy: i32,
    yx: i32,
    yy: i32,
    max_distance: i32,
) {
    if start_slope < end_slope {
        return;
    }
    let mut start_slope = start_slope;
    let mut next_start = 0.0f64;
    let mut blocked = false;
    let mut distance = row;
    while distance <= max_distance && !blocked {
        let dy = -distance;
        let mut dx = first_shadow_column(distance, start_slope);
        while dx <= 0 {
            let right_slope = (dx as f64 + 0.5) / (dy as f64 - 0.5);
            // start_slope may change inside this row when a blocked run ends.
            if start_slope < right_slope {
                dx += 1;
                continue;
            }
            let left_slope = (dx as f64 - 0.5) / (dy as f64 + 0.5);
            if end_slope > left_slope {
                break;
            }

            let cell_x = source_cell_x + dx * xx + dy * xy;
            let cell_y = source_cell_y + dx * yx + dy * yy;
            let delta_x = (cell_x - source_cell_x) * env.cell_size;
            let delta_y = (cell_y - source_cell_y) * env.cell_size;
            let distance_sq = delta_x as i64 * delta_x as i64 + delta_y as i64 * delta_y as i64;
            let visible_shape = env.shape.contains(delta_x, delta_y);
            if visible_shape && distance_sq > env.minimum_light_distance_sq {
                env.mark_lit(acc, cell_x, cell_y);
            }
            // Occlusion is deliberately NOT gated on visible_shape.
            //
            // Terrain blocks sight because it is terrain, not because we
            // happened to be lighting the cell it sits in. Requiring
            // visible_shape here made a wall just outside the lit shape
            // transparent, so light walked through it and re-emerged wherever
            // the shape widened again. This is the cost of the octant
            // slope-range optimisation -- it lets a cone visit only its own
            // slopes, but the cells it visits there must still shadow
            // honestly.
            let opaque = distance_sq > env.minimum_opaque_distance_sq
                && env.solid_block(cell_x * env.cell_size, cell_y * env.cell_size, env.cell_size);

            if blocked {
                if opaque {
                    next_start = right_slope;
                    dx += 1;
                    continue;
                }
                blocked = false;
                start_slope = next_start;
            } else if opaque && distance < max_distance {
                blocked = true;
                cast_shadow_octant(
                    env,
                    acc,
                    source_cell_x,
                    source_cell_y,
                    distance + 1,
                    start_slope,
                    left_slope,
                    xx,
                    xy,
                    yx,
                    yy,
                    max_distance,
                );
                next_start = right_slope;
            }
            dx += 1;
        }
        distance += 1;
    }
}

#[derive(Clone, Copy)]
struct ShadowSlopeRange {
    start: f64,
    end: f64,
}

/// Return the sub-slopes of one 45-degree octant that lie inside the cone.
/// Intersecting the two cone edges with the octant's 0..1 slope coordinate
/// avoids walking up to 135 degrees of distant terrain for a 90-degree cone.
fn cone_slope_ranges(
    shape: &ShadowVisionShape,
    octant: &[i32; 4],
    ranges: &mut [ShadowSlopeRange; 2],
) -> usize {
    if shape.cone_width <= 0 || shape.cone_length <= 0 {
        return 0;
    }
    if shape.cone_width >= 360 {
        ranges[0] = ShadowSlopeRange {
            start: 1.0,
            end: 0.0,
        };
        return 1;
    }

    const EPSILON: f64 = 1e-9;
    let cone_centre = (shape.cone_dir_y as f64).atan2(shape.cone_dir_x as f64);
    let half_angle = shape.cone_width as f64 * std::f64::consts::PI / 360.0;
    let edge_angles = [cone_centre - half_angle, cone_centre + half_angle];
    let mut points = [0.0f64; 4];
    points[0] = 0.0;
    points[1] = 1.0;
    let mut point_count = 2;

    // An octant ray at slope u has direction (-u*xx - xy, -u*yx - yy). Add
    // every u at which it is collinear with a cone edge, then test the
    // intervals between those crossings.
    for edge_angle in edge_angles {
        let edge_x = edge_angle.cos();
        let edge_y = edge_angle.sin();
        let denominator = -(octant[0] as f64) * edge_y + octant[2] as f64 * edge_x;
        if denominator.abs() <= EPSILON {
            continue;
        }
        let u = (octant[1] as f64 * edge_y - octant[3] as f64 * edge_x) / denominator;
        if u > EPSILON && u < 1.0 - EPSILON {
            points[point_count] = u;
            point_count += 1;
        }
    }
    points[..point_count].sort_by(|a, b| a.partial_cmp(b).unwrap());

    let mut range_count = 0;
    for i in 0..point_count - 1 {
        let low = points[i];
        let high = points[i + 1];
        if high - low <= EPSILON {
            continue;
        }
        let middle = (low + high) * 0.5;
        let direction_x = -middle * octant[0] as f64 - octant[1] as f64;
        let direction_y = -middle * octant[2] as f64 - octant[3] as f64;
        if !shape.direction_in_cone(direction_x, direction_y) {
            continue;
        }
        if range_count > 0 && (ranges[range_count - 1].start - low).abs() <= EPSILON {
            ranges[range_count - 1].start = high;
        } else {
            ranges[range_count] = ShadowSlopeRange {
                start: high,
                end: low,
            };
            range_count += 1;
        }
    }
    range_count
}

/// accumulators[0] collects the marks made directly; accumulators[1 + octant]
/// collect each octant's. The caller folds all of them into the mask once the
/// octants have joined.
#[allow(clippy::too_many_arguments)]
fn stamp_shadow_vision_inner(
    env: &CastEnv,
    accumulators: &mut [CastAccumulator; 1 + SHADOW_OCTANT_COUNT],
    pool: Option<&CastPool>,
    source: &VisionSource,
    circle_radius: i32,
    cone_width: i32,
    cone_length: i32,
    source_cell_x: i32,
    source_cell_y: i32,
    parallel: bool,
) {
    let shape = build_vision_shape(source, circle_radius, cone_width, cone_length);
    if shape.max_radius <= 0 {
        env.mark_lit(&mut accumulators[0], source_cell_x, source_cell_y);
        return;
    }
    if env.minimum_light_distance_sq == 0 {
        env.mark_lit(&mut accumulators[0], source_cell_x, source_cell_y);
    }
    // Match rays blocked at step zero: the worm cell is visible, but no sight
    // can propagate from a worm embedded in collision terrain.
    if (env.solid)(source.x, source.y) {
        return;
    }
    let circle_max_distance = (shape.circle_radius + env.cell_size - 1) / env.cell_size;
    let cone_max_distance = (shape.cone_length + env.cell_size - 1) / env.cell_size;

    // One work item per octant. Each writes only to its own accumulator and
    // to the shared stamp bitmap, so no item can observe another's output.
    // Capture the accumulator base as a raw pointer (Copy) so the closure
    // stays `Fn`: writing through a pointer does not borrow the environment.
    // The wrapper asserts the disjoint-access invariant the pool relies on:
    // work item `index` only ever touches slot `1 + index`.
    struct AccBase(*mut CastAccumulator);
    unsafe impl Send for AccBase {}
    unsafe impl Sync for AccBase {}
    impl Clone for AccBase {
        fn clone(&self) -> Self {
            *self
        }
    }
    impl Copy for AccBase {}
    impl AccBase {
        /// The accumulator for one octant. Each octant writes to its OWN
        /// accumulator; indices are disjoint across work items, so this alias
        /// is sound.
        #[inline]
        unsafe fn at(self, index: usize) -> &'static mut CastAccumulator {
            &mut *self.0.add(1 + index)
        }
    }
    let acc_base = AccBase(accumulators.as_mut_ptr());
    let cast_octant = |index: usize| {
        let octant = &OCTANTS[index];
        let acc = unsafe { acc_base.at(index) };
        if shape.circle_radius > 0 {
            cast_shadow_octant(
                env,
                acc,
                source_cell_x,
                source_cell_y,
                1,
                1.0,
                0.0,
                octant[0],
                octant[1],
                octant[2],
                octant[3],
                circle_max_distance,
            );
        }
        if shape.cone_length > shape.circle_radius && shape.cone_width > 0 {
            let mut ranges = [ShadowSlopeRange {
                start: 0.0,
                end: 0.0,
            }; 2];
            let range_count = cone_slope_ranges(&shape, octant, &mut ranges);
            for range in ranges.iter().take(range_count) {
                cast_shadow_octant(
                    env,
                    acc,
                    source_cell_x,
                    source_cell_y,
                    1,
                    range.start,
                    range.end,
                    octant[0],
                    octant[1],
                    octant[2],
                    octant[3],
                    cone_max_distance,
                );
            }
        }
    };

    if parallel {
        if let Some(pool) = pool {
            pool.run(SHADOW_OCTANT_COUNT, &cast_octant);
            return;
        }
    }
    for index in 0..SHADOW_OCTANT_COUNT {
        cast_octant(index);
    }
}

// ---------------------------------------------------------------------------
// Mask build: begin / finish / order, and the no-LOS fill.
// ---------------------------------------------------------------------------

fn pixels_to_grid_floor(pixels: i32, cell_size: i32) -> i32 {
    if pixels >= 0 {
        pixels / cell_size
    } else {
        -(((-pixels) + cell_size - 1) / cell_size)
    }
}

fn pixels_to_grid_ceil(pixels: i32, cell_size: i32) -> i32 {
    if pixels >= 0 {
        (pixels + cell_size - 1) / cell_size
    } else {
        -((-pixels) / cell_size)
    }
}

fn mask_chunk_floor(cell: i32) -> i32 {
    if cell >= 0 {
        (cell / CHUNK_CELLS) * CHUNK_CELLS
    } else {
        -(((-cell + CHUNK_CELLS - 1) / CHUNK_CELLS) * CHUNK_CELLS)
    }
}

fn begin_source_mask(
    mask: &mut SourceMask,
    scratch: &mut CastScratch,
    source: &VisionSource,
    extent_cells: i32,
    cell_size: i32,
) {
    let extent_cells = extent_cells.max(0);
    mask.source = source.clone();
    mask.cell_size = cell_size;
    mask.origin_x = pixels_to_grid_floor(source.x, cell_size) - extent_cells;
    mask.origin_y = pixels_to_grid_floor(source.y, cell_size) - extent_cells;
    mask.width = extent_cells * 2 + 1;
    mask.height = extent_cells * 2 + 1;
    mask.cells.clear();
    mask.cells_sorted = false;
    mask.lit_left = 0;
    mask.lit_top = 0;
    mask.lit_right = 0;
    mask.lit_bottom = 0;
    mask.aperture_valid = false;
    scratch.begin((mask.width as usize) * (mask.height as usize));
}

/// Merge the per-thread cast output back into the mask. The cell vector is
/// only sized here; order_source_mask_cells_from_scratch fills it from the
/// stamp bitmap and shrinks it to the true count.
fn finish_source_mask(mask: &mut SourceMask, accumulators: &[CastAccumulator]) {
    let mut stamped = 0usize;
    let mut any = false;
    for accumulator in accumulators {
        stamped += accumulator.stamped;
        if !accumulator.any {
            continue;
        }
        if !any {
            mask.lit_left = accumulator.lit_left;
            mask.lit_top = accumulator.lit_top;
            mask.lit_right = accumulator.lit_right;
            mask.lit_bottom = accumulator.lit_bottom;
            any = true;
        } else {
            mask.lit_left = mask.lit_left.min(accumulator.lit_left);
            mask.lit_top = mask.lit_top.min(accumulator.lit_top);
            mask.lit_right = mask.lit_right.max(accumulator.lit_right);
            mask.lit_bottom = mask.lit_bottom.max(accumulator.lit_bottom);
        }
    }
    if !any {
        mask.lit_left = 0;
        mask.lit_top = 0;
        mask.lit_right = 0;
        mask.lit_bottom = 0;
    }
    mask.cells.resize(stamped, SourceCell::default());
}

/// Put a just-rasterised mask in world row order. The generation bitmap is
/// already laid out that way, so one sequential scan is cheaper than sorting.
/// The ordered masks can then be merged like sets when a moving source's
/// vision replaces its previous result.
fn order_source_mask_cells_from_scratch(mask: &mut SourceMask, scratch: &CastScratch) {
    if mask.cells.is_empty() {
        mask.cells_sorted = true;
        return;
    }
    let first_x = mask.lit_left - mask.origin_x;
    let last_x = mask.lit_right - mask.origin_x;
    let first_y = mask.lit_top - mask.origin_y;
    let last_y = mask.lit_bottom - mask.origin_y;
    let mut output = 0usize;
    for local_y in first_y..last_y {
        let row = (local_y as usize) * (mask.width as usize);
        for local_x in first_x..last_x {
            if !scratch.is_stamped(row + local_x as usize) {
                continue;
            }
            mask.cells[output] = SourceCell {
                local_x: local_x as u16,
                local_y: local_y as u16,
            };
            output += 1;
        }
    }
    mask.cells.truncate(output);
    mask.cells_sorted = true;
}

fn source_mask_contains_world_cell(mask: &SourceMask, cell_x: i32, cell_y: i32) -> bool {
    if !mask.cells_sorted || mask.cells.is_empty() {
        return false;
    }
    let local_x = cell_x - mask.origin_x;
    let local_y = cell_y - mask.origin_y;
    if local_x < 0
        || local_x >= mask.width
        || local_y < 0
        || local_y >= mask.height
    {
        return false;
    }
    let target = (local_y as usize) * (mask.width as usize) + local_x as usize;
    let mut first = 0usize;
    let mut last = mask.cells.len();
    while first < last {
        let middle = first + (last - first) / 2;
        let cell = &mask.cells[middle];
        let value = (cell.local_y as usize) * (mask.width as usize) + cell.local_x as usize;
        if value < target {
            first = middle + 1;
        } else {
            last = middle;
        }
    }
    if first >= mask.cells.len() {
        return false;
    }
    let cell = &mask.cells[first];
    cell.local_x as i32 == local_x && cell.local_y as i32 == local_y
}

/// los = false: light the whole circle-plus-cone shape, with no occlusion.
/// With no terrain test there is nothing to occlude, so the answer is just
/// "every cell inside the shape" -- a bounded scan of the shape's own
/// bounding box, with ShadowVisionShape::contains doing the per-cell test so
/// this cannot drift from the shape the casting paths produce.
// The argument list mirrors Vision.cpp's fillVisionShape for review
// fidelity; bundling it further would only obscure the port.
#[allow(clippy::too_many_arguments)]
fn fill_vision_shape(
    env: &CastEnv,
    acc: &mut CastAccumulator,
    source: &VisionSource,
    circle_radius: i32,
    cone_width: i32,
    cone_length: i32,
    source_cell_x: i32,
    source_cell_y: i32,
) {
    let shape = build_vision_shape(source, circle_radius, cone_width, cone_length);
    if shape.max_radius <= 0 {
        env.mark_lit(acc, source_cell_x, source_cell_y);
        return;
    }

    // Bound each row before testing it, rather than scanning the whole
    // bounding box. The cone's own x/y extent is computed once, from the only
    // points that can be extreme: the apex, the two edge rays' endpoints, and
    // the arc's crossing of each axis. Every bound here is an OUTER bound and
    // shape.contains still decides each cell, so being generous costs time
    // and never correctness.
    let extent_cells = pixels_to_grid_ceil(shape.max_radius, env.cell_size);
    let cone_can_reach = shape.cone_width > 0 && shape.cone_length > 0;

    let mut cone_min_x = 0.0f64;
    let mut cone_max_x = 0.0f64;
    let mut cone_min_y = 0.0f64;
    let mut cone_max_y = 0.0f64;
    if cone_can_reach {
        let reach = shape.cone_length as f64;
        let centre = (shape.cone_dir_y as f64).atan2(shape.cone_dir_x as f64);
        let half = shape.cone_width as f64 * std::f64::consts::PI / 360.0;
        let mut include = |angle: f64| {
            let px = reach * angle.cos();
            let py = reach * angle.sin();
            cone_min_x = cone_min_x.min(px);
            cone_max_x = cone_max_x.max(px);
            cone_min_y = cone_min_y.min(py);
            cone_max_y = cone_max_y.max(py);
        };
        include(centre - half);
        include(centre + half);
        // Axis crossings: where the arc reaches further than either edge.
        for k in -4..=4 {
            let axis = k as f64 * (std::f64::consts::PI / 2.0);
            let mut delta = axis - centre;
            while delta > std::f64::consts::PI {
                delta -= 2.0 * std::f64::consts::PI;
            }
            while delta < -std::f64::consts::PI {
                delta += 2.0 * std::f64::consts::PI;
            }
            if delta.abs() <= half {
                include(axis);
            }
        }
    }

    for dy in -extent_cells..=extent_cells {
        let delta_y = dy * env.cell_size;
        let delta_y_sq = delta_y as i64 * delta_y as i64;

        let mut any = false;
        let mut lo_px = 0i32;
        let mut hi_px = 0i32;
        if delta_y_sq <= shape.circle_radius_sq {
            let span = ((shape.circle_radius_sq - delta_y_sq) as f64).sqrt() as i32;
            lo_px = -span;
            hi_px = span;
            any = true;
        }
        if cone_can_reach
            && delta_y_sq <= shape.cone_length_sq
            && delta_y as f64 >= cone_min_y - 1.0
            && delta_y as f64 <= cone_max_y + 1.0
        {
            let reach_span = ((shape.cone_length_sq - delta_y_sq) as f64).sqrt() as i32;
            let cone_lo = (-reach_span).max(cone_min_x.floor() as i32);
            let cone_hi = reach_span.min(cone_max_x.ceil() as i32);
            if cone_lo <= cone_hi {
                lo_px = if any { lo_px.min(cone_lo) } else { cone_lo };
                hi_px = if any { hi_px.max(cone_hi) } else { cone_hi };
                any = true;
            }
        }
        if !any {
            continue; // row lies outside both shapes
        }

        let lo_cell = lo_px / env.cell_size - 1;
        let hi_cell = hi_px / env.cell_size + 1;
        for dx in lo_cell..=hi_cell {
            if shape.contains(dx * env.cell_size, delta_y) {
                env.mark_lit(acc, source_cell_x + dx, source_cell_y + dy);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Far aperture derivation and the far-mask angular clip.
// ---------------------------------------------------------------------------

/// Emit one wedge of admitted directions. Bin b covers half a bin width
/// either side of its own direction, so a run of open bins reaches half a bin
/// past each end. `first_bin` may run past the direction count: it is only
/// ever used as an angle, and angles are periodic.
fn append_aperture_wedge(aperture: &mut FarAperture, first_bin: usize, bin_count: usize) {
    const TWO_PI: f64 = std::f64::consts::TAU;
    let step = TWO_PI / APERTURE_DIRECTIONS as f64;
    let start_angle = (first_bin as f64 - 0.5) * step;
    let end_angle = (first_bin as f64 + bin_count as f64 - 0.5) * step;
    let start_y = start_angle.sin();
    let end_y = end_angle.sin();
    // Half-bin boundaries of an even direction count can never be horizontal;
    // the guard is here so a future count change fails safe rather than
    // dividing by zero.
    if start_y == 0.0 || end_y == 0.0 {
        return;
    }
    aperture.wedges.push(FarWedge {
        start_cotangent: (start_angle.cos() / start_y) as f32,
        end_cotangent: (end_angle.cos() / end_y) as f32,
        start_is_upper_bound: start_y > 0.0,
        end_is_lower_bound: end_y > 0.0,
    });
}

/// Derive the far aperture from an exact near mask: which directions still
/// reach its rim unobstructed. MUST be called immediately after
/// order_source_mask_cells_from_scratch for this mask, while the scratch
/// still holds its stamp. That gives each rim probe an O(1) lookup.
// `bin` drives both the probe angle and the bin store; an iterator would
// split the two uses for no readability gain.
#[allow(clippy::needless_range_loop)]
fn build_far_aperture(
    mask: &mut SourceMask,
    scratch: &CastScratch,
    probe_radius: i32,
    solid: &(dyn Fn(i32, i32) -> bool + Sync),
) {
    mask.aperture.x = mask.source.x;
    mask.aperture.y = mask.source.y;
    mask.aperture.wedges.clear();
    mask.aperture.restricted = false;
    mask.aperture_valid = false;
    if probe_radius <= 0 || mask.cell_size != 1 || mask.cells.is_empty() || !mask.cells_sorted
    {
        return;
    }

    let mut open_bins = [0u8; APERTURE_DIRECTIONS];
    let mut open_count = 0usize;
    const TWO_PI: f64 = std::f64::consts::TAU;
    for bin in 0..APERTURE_DIRECTIONS {
        let angle = TWO_PI * bin as f64 / APERTURE_DIRECTIONS as f64;
        let x = mask.source.x + (angle.cos() * probe_radius as f64).round() as i32;
        let y = mask.source.y + (angle.sin() * probe_radius as f64).round() as i32;
        let local_x = x - mask.origin_x;
        let local_y = y - mask.origin_y;
        let in_mask = local_x >= 0
            && local_x < mask.width
            && local_y >= 0
            && local_y < mask.height
            && scratch.is_stamped(
                (local_y as usize) * (mask.width as usize) + local_x as usize,
            );
        // The shadowcaster lights the cell that blocks it, so a lit but solid
        // probe is a wall face, not a direction sight passes through.
        let lit = in_mask && !solid(x, y);
        open_bins[bin] = lit as u8;
        open_count += lit as usize;
    }
    mask.aperture_valid = true;
    // Open sky: no restriction to express, and the composite can skip the
    // whole clip for this frame.
    if open_count == APERTURE_DIRECTIONS {
        return;
    }
    mask.aperture.restricted = true;
    // A worm buried in terrain sees nothing past its own rim.
    if open_count == 0 {
        return;
    }

    // Start from a blocked bin so a run wrapping past direction zero is
    // emitted once, as one wedge.
    let mut first = 0usize;
    while open_bins[first] != 0 {
        first += 1;
    }
    let mut offset = 0usize;
    while offset < APERTURE_DIRECTIONS {
        if open_bins[(first + offset) % APERTURE_DIRECTIONS] == 0 {
            offset += 1;
            continue;
        }
        let mut length = 0usize;
        while offset + length < APERTURE_DIRECTIONS
            && open_bins[(first + offset + length) % APERTURE_DIRECTIONS] != 0
        {
            length += 1;
        }
        let mut piece = 0usize;
        while piece < length {
            append_aperture_wedge(
                &mut mask.aperture,
                first + offset + piece,
                APERTURE_MAX_WEDGE_BINS.min(length - piece),
            );
            piece += APERTURE_MAX_WEDGE_BINS;
        }
        offset += length;
    }
}

/// A lower-resolution shadowcaster may quantise a terrain tangent to a less
/// restrictive angle than the exact near mask. That would make a shadowed
/// direction become visible again at the LOD boundary, which is impossible
/// for opaque terrain. Sample the exact mask just inside its outer circle and
/// use those samples as a conservative angular aperture for the far result.
fn clip_far_mask_to_near_angular_visibility(
    far_mask: &mut SourceMask,
    near_mask: &SourceMask,
    solid: &(dyn Fn(i32, i32) -> bool + Sync),
) {
    if far_mask.cell_size <= 1
        || near_mask.cell_size != 1
        || near_mask.width <= 1
        || near_mask.height <= 1
        || near_mask.cells.is_empty()
        || !near_mask.cells_sorted
    {
        return;
    }

    const SLOPE_BINS: usize = 512;
    const OCTANT_COUNT: usize = 8;
    const PROBE_MARGIN_PIXELS: i32 = 3;
    let mut visible_directions = [0u8; OCTANT_COUNT * SLOPE_BINS];
    let near_radius = (near_mask.width - 1) / 2;
    let probe_radius = near_radius - PROBE_MARGIN_PIXELS;
    if probe_radius <= 0 {
        return;
    }

    const MAJOR_X: [i32; OCTANT_COUNT] = [1, 0, 0, -1, -1, 0, 0, 1];
    const MAJOR_Y: [i32; OCTANT_COUNT] = [0, 1, 1, 0, 0, -1, -1, 0];
    const MINOR_X: [i32; OCTANT_COUNT] = [0, 1, -1, 0, 0, -1, 1, 0];
    const MINOR_Y: [i32; OCTANT_COUNT] = [1, 0, 0, 1, -1, 0, 0, -1];
    for octant in 0..OCTANT_COUNT {
        for bin in 0..SLOPE_BINS {
            let slope = (bin as f64 + 0.5) / SLOPE_BINS as f64;
            let major_length = probe_radius as f64 / (1.0 + slope * slope).sqrt();
            let major = major_length.round() as i32;
            let minor = (major_length * slope).round() as i32;
            let x = near_mask.source.x + MAJOR_X[octant] * major + MINOR_X[octant] * minor;
            let y = near_mask.source.y + MAJOR_Y[octant] * major + MINOR_Y[octant] * minor;
            visible_directions[octant * SLOPE_BINS + bin] =
                (source_mask_contains_world_cell(near_mask, x, y) && !solid(x, y)) as u8;
        }
    }

    let direction_bin = |delta_x: i32, delta_y: i32| -> usize {
        let abs_x = delta_x.abs();
        let abs_y = delta_y.abs();
        let (octant, major, minor);
        if delta_x >= 0 && delta_y >= 0 {
            if abs_x >= abs_y {
                octant = 0;
                major = abs_x;
                minor = abs_y;
            } else {
                octant = 1;
                major = abs_y;
                minor = abs_x;
            }
        } else if delta_x < 0 && delta_y >= 0 {
            if abs_y >= abs_x {
                octant = 2;
                major = abs_y;
                minor = abs_x;
            } else {
                octant = 3;
                major = abs_x;
                minor = abs_y;
            }
        } else if delta_x < 0 {
            if abs_x >= abs_y {
                octant = 4;
                major = abs_x;
                minor = abs_y;
            } else {
                octant = 5;
                major = abs_y;
                minor = abs_x;
            }
        } else if abs_y >= abs_x {
            octant = 6;
            major = abs_y;
            minor = abs_x;
        } else {
            octant = 7;
            major = abs_x;
            minor = abs_y;
        }
        if major <= 0 {
            return 0;
        }
        let bin = (SLOPE_BINS - 1).min((minor as i64 * SLOPE_BINS as i64 / major as i64) as usize);
        octant * SLOPE_BINS + bin
    };

    let mut output = 0usize;
    let mut have_bounds = false;
    for i in 0..far_mask.cells.len() {
        let cell = far_mask.cells[i];
        let far_cell_x = far_mask.origin_x + cell.local_x as i32;
        let far_cell_y = far_mask.origin_y + cell.local_y as i32;
        let world_x = far_cell_x * far_mask.cell_size + far_mask.cell_size / 2;
        let world_y = far_cell_y * far_mask.cell_size + far_mask.cell_size / 2;
        let delta_x = world_x - near_mask.source.x;
        let delta_y = world_y - near_mask.source.y;
        if visible_directions[direction_bin(delta_x, delta_y)] == 0 {
            continue;
        }
        far_mask.cells[output] = cell;
        output += 1;
        if !have_bounds {
            far_mask.lit_left = far_cell_x;
            far_mask.lit_right = far_cell_x;
            far_mask.lit_top = far_cell_y;
            far_mask.lit_bottom = far_cell_y;
            have_bounds = true;
        } else {
            far_mask.lit_left = far_mask.lit_left.min(far_cell_x);
            far_mask.lit_top = far_mask.lit_top.min(far_cell_y);
            far_mask.lit_right = far_mask.lit_right.max(far_cell_x);
            far_mask.lit_bottom = far_mask.lit_bottom.max(far_cell_y);
        }
    }
    far_mask.cells.truncate(output);
    if have_bounds {
        far_mask.lit_right += 1;
        far_mask.lit_bottom += 1;
    } else {
        far_mask.lit_left = 0;
        far_mask.lit_top = 0;
        far_mask.lit_right = 0;
        far_mask.lit_bottom = 0;
    }
}

// ---------------------------------------------------------------------------
// Live explosions.
// ---------------------------------------------------------------------------

/// One call to the terrain hook: a real crater centre and radius, rather than
/// one "explosion" as a player would count them. A cluster bomb's pellets and
/// an airstrike's bombing run each fire the hook several times, and taking
/// them as they come gives the true footprint for free.
struct LiveExplosion {
    x: i32,
    y: i32,
    /// Crater radius plus the pad, i.e. the radius at full brightness.
    radius: i32,
    start: std::time::Instant,
    /// Identity for mask caching. A counter rather than a position hash: two
    /// blasts can share a centre, and reusing the key would make the second
    /// reuse the first's mask.
    id: u32,
}

// ---------------------------------------------------------------------------
// Feather regions and the tile-based feather pass.
// ---------------------------------------------------------------------------

/// Feathering is only affected within two cells of a changed hard source.
/// Rebuilding those small source bounds avoids repeatedly scanning a
/// 32-million-cell large-map mask while a rope-moving worm changes vision.
#[derive(Clone, Copy, Default)]
struct FeatherRegion {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32, // exclusive
    /// Sparse invalidation tiles already describe the exact output to
    /// rebuild. Source/view bounds retain the legacy expansion by the active
    /// halo width.
    exact_output: bool,
}

fn is_empty_region(r: &FeatherRegion) -> bool {
    r.left >= r.right || r.top >= r.bottom
}

const FEATHER_TILE_SHIFT: i32 = 5;
const FEATHER_TILE_SIZE: i32 = 1 << FEATHER_TILE_SHIFT;

// ---------------------------------------------------------------------------
// Fog
// ---------------------------------------------------------------------------

/// The fog-of-war visibility engine. One per map; `update` is called once per
/// render frame with that frame's vision sources and a solidity closure over
/// the landscape's 1bpp collision grid.
pub struct Fog {
    scheme: Scheme,
    map_w: usize,
    map_h: usize,

    // Native-resolution light grid. One byte covers exactly one landscape
    // pixel, so terrain and fog share the same edge scale.
    grid_w: i32,
    grid_h: i32,
    origin_cell_x: i32,
    origin_cell_y: i32,
    limit_cell_x: i32,
    light: Vec<u8>,
    /// Number of cached source masks covering each cell. Keeping the count
    /// lets one moving worm remove its old contribution without erasing a
    /// cell still seen by an idle teammate.
    light_coverage: Vec<u8>,
    /// The same count, but only over sources allowed to DISCOVER terrain
    /// (worms and owned animals). A projectile lights the ground for everyone
    /// on purpose, but terrain memory is permanent for the round, so
    /// recording from that light would let an opponent shell your side of the
    /// map and leave you a free permanent survey of it.
    discover_coverage: Vec<u8>,
    /// The discovering union as a LIGHT LEVEL: 0 or 255, never a count. This
    /// is what `Discovery::record` reads.
    discover_light: Vec<u8>,

    // The far cone is stored in its own conservative coarse grid. The exact
    // native grid above still owns the circle around every source.
    far_grid_w: i32,
    far_grid_h: i32,
    far_origin_cell_x: i32,
    far_origin_cell_y: i32,
    far_limit_cell_x: i32,
    far_cell_size: i32,
    far_light: Vec<u8>,
    far_light_coverage: Vec<u8>,
    far_discover_coverage: Vec<u8>,
    far_discover_light: Vec<u8>,

    // Per-source cached masks, native and far tiers.
    source_masks: Vec<SourceMask>,
    far_source_masks: Vec<SourceMask>,
    /// This frame's far apertures, rebuilt from the live near masks at the
    /// end of every update. Empty means "admit far light everywhere".
    far_apertures: Vec<FarAperture>,

    // Cast scratch, reused across frames and sources.
    scratch: CastScratch,
    pool: CastPool,
    /// Replacement needs old and new native masks alive together. Recycle the
    /// displaced cell vector so a moving worm does not allocate it every
    /// frame.
    native_mask_build_cells: Vec<SourceCell>,
    /// Native hard-union transitions produced by exact old/new mask merges,
    /// converted to deduplicated feather tiles after every source update.
    native_union_changes: Vec<(i32, i32)>,

    // Feather scratch, reused by the local feather pass. They pack eight
    // cells per byte and are only as large as a changed source's vision
    // rectangle, never the landscape.
    feather_hard_bits: Vec<u8>,
    feather_near_bits: Vec<u8>,
    feather_far_bits: Vec<u8>,
    feather_dirty_tile_marks: Vec<u32>,
    feather_dirty_tile_indices: Vec<i32>,
    feather_dirty_tile_generation: u32,
    feather_tile_columns: i32,
    feather_tile_rows: i32,

    // Cached settings-derived state, for change detection.
    geometry: [SourceGeometry; SOURCE_KIND_COUNT],
    cached_geometry: [SourceGeometry; SOURCE_KIND_COUNT],
    cached_view_ownership: i32,

    empty_source_frames: u32,
    explosions: Vec<LiveExplosion>,
    next_explosion_id: u32,

    /// Union of terrain that changed since the last update, in landscape
    /// pixels, half-open. Only sources whose lit bounds overlap it need
    /// re-casting; `terrain_dirty_all` covers changes whose extent is unknown
    /// (new map, scheme change), where everything must rebuild. Mirrors
    /// wkFOW's `g_terrainDirtyRegion` / `g_terrainDirtyAll`.
    terrain_dirty_all: bool,
    terrain_dirty_region: bool,
    terrain_dirty_left: i32,
    terrain_dirty_top: i32,
    terrain_dirty_right: i32,
    terrain_dirty_bottom: i32,

    discovery: Discovery,
}

impl Fog {
    /// Create a fog engine for a `map_w` x `map_h` landscape. Dimensions are
    /// sanity-bounded (MAX_MAP_DIM / MAX_MAP_PIXELS); an insane dimension
    /// clamps the allocation to nothing and `update` becomes a no-op.
    pub fn new(map_w: usize, map_h: usize, scheme: Scheme) -> Self {
        let sane = map_dimensions_sane(map_w, map_h);
        let (map_w, map_h) = if sane { (map_w, map_h) } else { (0, 0) };
        let discover_mode = Mode::from_i32(scheme.get("discover").unwrap_or(1));
        let mut fog = Fog {
            scheme,
            map_w,
            map_h,
            grid_w: 0,
            grid_h: 0,
            origin_cell_x: 0,
            origin_cell_y: 0,
            limit_cell_x: 0,
            light: Vec::new(),
            light_coverage: Vec::new(),
            discover_coverage: Vec::new(),
            discover_light: Vec::new(),
            far_grid_w: 0,
            far_grid_h: 0,
            far_origin_cell_x: 0,
            far_origin_cell_y: 0,
            far_limit_cell_x: 0,
            far_cell_size: FAR_FIELD_CELL_SIZE,
            far_light: Vec::new(),
            far_light_coverage: Vec::new(),
            far_discover_coverage: Vec::new(),
            far_discover_light: Vec::new(),
            source_masks: Vec::new(),
            far_source_masks: Vec::new(),
            far_apertures: Vec::new(),
            scratch: CastScratch::new(),
            pool: CastPool::new(),
            native_mask_build_cells: Vec::new(),
            native_union_changes: Vec::new(),
            feather_hard_bits: Vec::new(),
            feather_near_bits: Vec::new(),
            feather_far_bits: Vec::new(),
            feather_dirty_tile_marks: Vec::new(),
            feather_dirty_tile_indices: Vec::new(),
            feather_dirty_tile_generation: 0,
            feather_tile_columns: 0,
            feather_tile_rows: 0,
            geometry: [SourceGeometry::default(); SOURCE_KIND_COUNT],
            cached_geometry: [SourceGeometry::default(); SOURCE_KIND_COUNT],
            cached_view_ownership: -1,
            empty_source_frames: 0,
            explosions: Vec::new(),
            next_explosion_id: 1,
            // A fresh engine has never seen the map: every first mask is a
            // full cast, which is what `terrain_dirty_all` already forces.
            terrain_dirty_all: true,
            terrain_dirty_region: false,
            terrain_dirty_left: 0,
            terrain_dirty_top: 0,
            terrain_dirty_right: 0,
            terrain_dirty_bottom: 0,
            discovery: Discovery::new(map_w, map_h, discover_mode),
        };
        fog.allocate_grids();
        fog
    }

    pub fn scheme(&self) -> &Scheme {
        &self.scheme
    }

    /// Replace the scheme. If any vision geometry changed, every cached mask
    /// is invalidated (masks from different kinds share the light union, so a
    /// partial rebuild would leave the other kinds' contributions
    /// double-counted there).
    pub fn set_scheme(&mut self, scheme: Scheme) {
        let old = self.resolve_geometry();
        let old_ownership = self.scheme.get("view_ownership").unwrap_or(1);
        self.scheme = scheme;
        let new = self.resolve_geometry();
        let new_ownership = self.scheme.get("view_ownership").unwrap_or(1);
        self.discovery
            .set_mode(Mode::from_i32(self.scheme.get("discover").unwrap_or(1)));
        if old != new || old_ownership != new_ownership {
            self.invalidate_all_masks();
        }
    }

    pub fn discovery(&self) -> &Discovery {
        &self.discovery
    }

    /// Notify the fog that an explosion happened at (x, y) with the given
    /// crater radius. The reveal circle is the crater radius plus the
    /// `explosion_circle` pad, and fades on a wall clock over
    /// EXPLOSION_FADE_MS with the radius following `full*(1-t^12)`.
    pub fn notify_explosion(&mut self, x: i32, y: i32, radius: i32) {
        let pad = self.scheme.get("explosion_circle").unwrap_or(40);
        if pad <= 0 {
            return;
        }
        let radius = radius.max(0) + pad;
        // Drop the OLDEST when full. A burst that overruns the cap is an
        // airstrike or a cluster, where the last craters are the ones still
        // being drawn and the first are already fading.
        if self.explosions.len() >= EXPLOSION_MAX_LIVE {
            self.explosions.remove(0);
        }
        let id = self.next_explosion_id;
        self.next_explosion_id += 1;
        self.explosions.push(LiveExplosion {
            x,
            y,
            radius,
            start: std::time::Instant::now(),
            id,
        });
        // The blast exposed ground that is now public knowledge even after
        // its light fades. Record the crater (not the wider pad) permanently.
        let map_w = self.map_w;
        let map_h = self.map_h;
        self.discovery.notify_crater(x, y, radius.max(0), move |px, py| {
            // We have no solidity closure here; the crater's terrain is by
            // definition gone. Treat only in-bounds pixels, solid=false: the
            // discovery floor for the crater is about showing the hole, and
            // the next update() with real light records the rim properly.
            let _ = (map_w, map_h);
            let _ = (px, py);
            false
        });
    }

    /// Notify the fog that the collision grid changed in a rectangle of
    /// landscape pixels (half-open). Sources whose lit bounds overlap the
    /// rectangle are re-cast on the next update; everything else keeps its
    /// cached mask. This is the engine's terrain-mutation signal — crater and
    /// land-write hooks call it so a blast under a stationary worm re-opens
    /// its sight lines. Mirrors wkFOW's `g_terrainDirtyRegion`.
    pub fn notify_terrain_changed(&mut self, left: i32, top: i32, right: i32, bottom: i32) {
        if right <= left || bottom <= top {
            return;
        }
        if self.terrain_dirty_region {
            self.terrain_dirty_left = self.terrain_dirty_left.min(left);
            self.terrain_dirty_top = self.terrain_dirty_top.min(top);
            self.terrain_dirty_right = self.terrain_dirty_right.max(right);
            self.terrain_dirty_bottom = self.terrain_dirty_bottom.max(bottom);
        } else {
            self.terrain_dirty_region = true;
            self.terrain_dirty_left = left;
            self.terrain_dirty_top = top;
            self.terrain_dirty_right = right;
            self.terrain_dirty_bottom = bottom;
        }
    }

    /// Notify the fog that the whole collision grid may have changed (new map,
    /// or a change whose extent is unknown). Every mask is re-cast next update.
    pub fn notify_terrain_changed_all(&mut self) {
        self.terrain_dirty_all = true;
    }

    /// Does a source mask's lit bounds (in cell coordinates, `cell_size` px per
    /// cell) overlap the pending terrain-dirty rectangle?
    fn mask_overlaps_terrain_dirty(&self, mask: &SourceMask) -> bool {
        if !self.terrain_dirty_region {
            return false;
        }
        let cs = mask.cell_size;
        let left = mask.lit_left * cs;
        let top = mask.lit_top * cs;
        let right = mask.lit_right * cs;
        let bottom = mask.lit_bottom * cs;
        left < self.terrain_dirty_right
            && right > self.terrain_dirty_left
            && top < self.terrain_dirty_bottom
            && bottom > self.terrain_dirty_top
    }

    /// Native light value (0/96/192/255) at a landscape pixel, including the
    /// discovery floor. This is what the engine samples to build the screen
    /// mask. Out-of-bounds pixels are fully hidden.
    pub fn light_at(&self, x: usize, y: usize) -> u8 {
        if x >= self.map_w || y >= self.map_h {
            return 0;
        }
        let cx = x as i32 - self.origin_cell_x;
        let cy = y as i32 - self.origin_cell_y;
        let mut value = 0u8;
        if cx >= 0 && cx < self.grid_w && cy >= 0 && cy < self.grid_h {
            let index = (cy as usize) * (self.grid_w as usize) + cx as usize;
            value = self.light[index];
        }
        // The far tier lights coarse cells; sample it where the native tier
        // is dark. Far light is hard (0/255) -- the feather only rings the
        // native union.
        if value < 255 && self.far_cell_size > 1 && !self.far_light.is_empty() {
            let fx = x as i32 / self.far_cell_size - self.far_origin_cell_x;
            let fy = y as i32 / self.far_cell_size - self.far_origin_cell_y;
            if fx >= 0 && fx < self.far_grid_w && fy >= 0 && fy < self.far_grid_h {
                let index = (fy as usize) * (self.far_grid_w as usize) + fx as usize;
                if self.far_light[index] > value {
                    value = self.far_light[index];
                }
            }
        }
        let floor = self.discovery.floor_at(x, y);
        value.max(floor)
    }

    /// The hard union of sources allowed to write terrain memory, on the
    /// native grid: one byte per cell, 0 or 255 where a worm or owned animal
    /// can see. No feather, because Discovery thresholds at 200 and the
    /// feathered rim is below that.
    pub fn discover_light(&self) -> &[u8] {
        &self.discover_light
    }

    /// Native grid dimensions and origin, for interpreting `discover_light`.
    pub fn grid(&self) -> (i32, i32, i32, i32) {
        (self.grid_w, self.grid_h, self.origin_cell_x, self.origin_cell_y)
    }

    /// This frame's far apertures. Empty when there is no far field to clip,
    /// or when any source's rim could not be probed -- an unknown aperture
    /// must admit everything, never invent fog.
    pub fn far_apertures(&self) -> &[FarAperture] {
        &self.far_apertures
    }

    // -- internal -----------------------------------------------------------

    fn resolve_geometry(&self) -> [SourceGeometry; SOURCE_KIND_COUNT] {
        let s = &self.scheme;
        let get = |name: &str, default: i32| s.get(name).unwrap_or(default);
        let los = |name: &str, default: bool| {
            s.get(name).map(|v| v != 0).unwrap_or(default)
        };
        let mut g = [SourceGeometry::default(); SOURCE_KIND_COUNT];
        g[SourceKind::Worm.index()] = SourceGeometry {
            circle_radius: get("circle_radius", 250),
            cone_width: get("cone_width", 45),
            cone_length: get("cone_length", 0),
            los: los("los", true),
            native_detail_radius: 0,
            hybrid_far_active: false,
        };
        g[SourceKind::Animal.index()] = SourceGeometry {
            circle_radius: get("animal_circle_radius", 150),
            cone_width: get("animal_cone_width", 45),
            cone_length: get("animal_cone_length", 0),
            // Animals share the worm LOS rule; there is no separate setting.
            los: los("los", true),
            native_detail_radius: 0,
            hybrid_far_active: false,
        };
        g[SourceKind::Projectile.index()] = SourceGeometry {
            circle_radius: get("proj_circle", 5),
            cone_width: get("proj_cone_width", 45),
            cone_length: get("proj_cone_length", 0),
            los: los("proj_los", false),
            native_detail_radius: 0,
            hybrid_far_active: false,
        };
        // Circle-only and terrain-blocked by design: all peers get precisely
        // the same rule from crate_circle alone.
        g[SourceKind::Crate.index()] = SourceGeometry {
            circle_radius: get("crate_circle", 20),
            cone_width: 360,
            cone_length: 0,
            los: true,
            native_detail_radius: 0,
            hybrid_far_active: false,
        };
        // Circle-only, and NOT terrain-blocked. Line of sight is the wrong
        // question for a blast: an explosion has just removed whatever was in
        // the way. The radius here is only the PAD; each blast carries its
        // own crater radius on its source.
        g[SourceKind::Explosion.index()] = SourceGeometry {
            circle_radius: get("explosion_circle", 40),
            cone_width: 360,
            cone_length: 0,
            los: false,
            native_detail_radius: 0,
            hybrid_far_active: false,
        };
        g
    }

    fn invalidate_all_masks(&mut self) {
        self.source_masks.clear();
        self.far_source_masks.clear();
        self.far_apertures.clear();
        self.clear_light_union();
        self.clear_far_light_union();
        self.cached_view_ownership = -1;
    }

    fn clear_light_union(&mut self) {
        self.light.fill(0);
        self.light_coverage.fill(0);
        self.discover_coverage.fill(0);
        self.discover_light.fill(0);
    }

    fn clear_far_light_union(&mut self) {
        self.far_light.fill(0);
        self.far_light_coverage.fill(0);
        self.far_discover_coverage.fill(0);
        self.far_discover_light.fill(0);
    }

    fn allocate_grids(&mut self) {
        if self.map_w == 0 || self.map_h == 0 {
            return;
        }
        self.origin_cell_x = 0;
        self.origin_cell_y = 0;
        self.limit_cell_x = self.map_w as i32;
        self.grid_w = self.map_w as i32;
        self.grid_h = self.map_h as i32;
        let cells = self.map_w * self.map_h;
        self.light = vec![0; cells];
        self.light_coverage = vec![0; cells];
        self.discover_coverage = vec![0; cells];
        self.discover_light = vec![0; cells];

        let fw = pixels_to_grid_ceil(self.map_w as i32, self.far_cell_size);
        let fh = pixels_to_grid_ceil(self.map_h as i32, self.far_cell_size);
        self.far_grid_w = fw;
        self.far_grid_h = fh;
        self.far_origin_cell_x = 0;
        self.far_origin_cell_y = 0;
        self.far_limit_cell_x = fw;
        let far_cells = (fw as usize) * (fh as usize);
        self.far_light = vec![0; far_cells];
        self.far_light_coverage = vec![0; far_cells];
        self.far_discover_coverage = vec![0; far_cells];
        self.far_discover_light = vec![0; far_cells];
    }

    fn geometry_for(&self, kind: SourceKind) -> &SourceGeometry {
        &self.geometry[kind.index()]
    }

    fn circle_radius_for(&self, source: &VisionSource) -> i32 {
        if source.circle_radius >= 0 {
            source.circle_radius
        } else {
            self.geometry_for(source.kind).circle_radius
        }
    }

    fn extent_for(&self, source: &VisionSource) -> i32 {
        self.circle_radius_for(source)
            .max(self.geometry_for(source.kind).cone_length)
    }

    /// The cone length this source actually gets, per view_ownership. Mode 2
    /// (active only) gives only the active worm a cone; modes 1 and 3 give
    /// every worm a cone. Non-worm kinds simply get their own cone.
    fn effective_cone_length_for(&self, source: &VisionSource, cone_length: i32) -> i32 {
        if source.kind != SourceKind::Worm {
            return cone_length;
        }
        match self.scheme.get("view_ownership").unwrap_or(1) {
            0 => 0,
            2 => {
                if source.active {
                    cone_length
                } else {
                    0
                }
            }
            _ => cone_length,
        }
    }

    /// Where to probe a near mask's rim when deriving its far aperture. It
    /// must be a radius the NEAR MASK ACTUALLY LIT, and in hybrid mode the
    /// near cast stamps the circle and nothing else, so the ring sits inside
    /// circle_radius, not at native_detail_radius.
    fn aperture_probe_radius(geom: &SourceGeometry, circle_radius: i32) -> i32 {
        let rim = circle_radius.min(geom.native_detail_radius);
        rim - APERTURE_PROBE_MARGIN_PIXELS
    }

    /// One render frame. `sources` are this frame's vision sources (already
    /// ownership-filtered by the caller). `solid(x, y)` reads the 1bpp
    /// collision grid. Rebuilds only dirty source masks, unions into the
    /// light grid, records discovery, applies the feather.
    pub fn update(&mut self, sources: &[VisionSource], solid: impl Fn(i32, i32) -> bool + Sync) {
        if self.map_w == 0 || self.map_h == 0 {
            return;
        }
        let solid: &(dyn Fn(i32, i32) -> bool + Sync) = &solid;

        // This frame's settings, resolved per kind, plus the derived
        // two-tier LOD split.
        self.geometry = self.resolve_geometry();
        let mut any_hybrid_far_active = false;
        for geom in &mut self.geometry {
            // Where full resolution stops -- for the CIRCLE as well as the
            // cone. It bounds the whole reach, so both shapes drop to the
            // coarse tier past it. It is NOT the circle: the cast takes the
            // radius; this only decides where that cast changes resolution.
            geom.native_detail_radius = NATIVE_DETAIL_RADIUS.clamp(0, geom.extent());
            geom.hybrid_far_active =
                FAR_FIELD_CELL_SIZE > 1 && geom.extent() > geom.native_detail_radius;
            if geom.hybrid_far_active {
                any_hybrid_far_active = true;
            }
        }
        let view_ownership = self.scheme.get("view_ownership").unwrap_or(1);

        // Age and emit explosion reveals. Expiry happens here rather than in
        // notify_explosion because that may not be called again for seconds;
        // a reveal has to fade on the frames after the blast.
        let mut frame_sources: Vec<VisionSource> = Vec::with_capacity(sources.len() + 4);
        frame_sources.extend_from_slice(sources);
        if !self.explosions.is_empty() {
            let now = std::time::Instant::now();
            let lifetime = std::time::Duration::from_millis(EXPLOSION_FADE_MS);
            let mut live = 0usize;
            for i in 0..self.explosions.len() {
                let blast = &self.explosions[i];
                let elapsed = now.saturating_duration_since(blast.start);
                if elapsed >= lifetime {
                    continue;
                }
                // t runs 0..1 across the window, as in the C++ `elapsed /
                // EXPLOSION_FADE_MS`.
                let t = elapsed.as_secs_f64() * 1000.0 / EXPLOSION_FADE_MS as f64;
                // t^N on the faded fraction: the circle holds wide while the
                // smoke is thick, then closes decisively.
                let faded = t.powi(EXPLOSION_FADE_POWER as i32);
                let radius = (blast.radius as f64 * (1.0 - faded)) as i32;
                let blast = LiveExplosion {
                    x: blast.x,
                    y: blast.y,
                    radius: blast.radius,
                    start: blast.start,
                    id: blast.id,
                };
                self.explosions[live] = blast;
                live += 1;
                if radius <= 0 {
                    continue;
                }
                frame_sources.push(VisionSource {
                    kind: SourceKind::Explosion,
                    id: self.explosions[live - 1].id,
                    x: self.explosions[live - 1].x,
                    y: self.explosions[live - 1].y,
                    active: true,
                    circle_radius: radius,
                    aimable: true,
                    ..Default::default()
                });
            }
            self.explosions.truncate(live);
        }

        // Grow the canvas to cover every source's complete vision reach, in
        // 256px chunks so a flying worm does not reallocate every pixel.
        let mut required_left = 0i32;
        let mut required_right = self.map_w as i32;
        let mut required_top = 0i32;
        for source in &frame_sources {
            let vision_extent = self.extent_for(source) + 64;
            if source.x - vision_extent < required_left {
                required_left = source.x - vision_extent;
            }
            if source.x + vision_extent > required_right {
                required_right = source.x + vision_extent;
            }
            if source.y - vision_extent < required_top {
                required_top = source.y - vision_extent;
            }
        }
        let required_left_cell = pixels_to_grid_floor(required_left, 1);
        let required_right_cell = pixels_to_grid_ceil(required_right, 1);
        let required_top_cell = pixels_to_grid_floor(required_top, 1);
        let mut grid_changed = false;
        let new_origin_x = self.origin_cell_x.min(mask_chunk_floor(required_left_cell));
        let new_origin_y = self.origin_cell_y.min(mask_chunk_floor(required_top_cell));
        let new_limit_x = self
            .limit_cell_x
            .max(-mask_chunk_floor(-required_right_cell));
        let want_w = new_limit_x - new_origin_x;
        let want_h = self.map_h as i32 - new_origin_y;
        if new_origin_x != self.origin_cell_x
            || new_origin_y != self.origin_cell_y
            || want_w != self.grid_w
            || want_h != self.grid_h
        {
            // Canvas growth: cached source cells are world-space, so
            // replaying them fills the newly visible area without re-casting.
            let old_origin_x = self.origin_cell_x;
            let old_origin_y = self.origin_cell_y;
            let old_w = self.grid_w;
            let old_h = self.grid_h;
            let old_light = std::mem::take(&mut self.light);
            let old_coverage = std::mem::take(&mut self.light_coverage);
            let old_disc_cov = std::mem::take(&mut self.discover_coverage);
            let old_disc_light = std::mem::take(&mut self.discover_light);
            self.origin_cell_x = new_origin_x;
            self.origin_cell_y = new_origin_y;
            self.limit_cell_x = new_limit_x;
            self.grid_w = want_w;
            self.grid_h = want_h;
            let cells = (want_w as usize) * (want_h as usize);
            self.light = vec![0; cells];
            self.light_coverage = vec![0; cells];
            self.discover_coverage = vec![0; cells];
            self.discover_light = vec![0; cells];
            // Copy the old union into the new canvas at the shifted origin.
            if old_w > 0 && old_h > 0 {
                for y in 0..old_h {
                    let ny = y + (old_origin_y - new_origin_y);
                    if ny < 0 || ny >= want_h {
                        continue;
                    }
                    for x in 0..old_w {
                        let nx = x + (old_origin_x - new_origin_x);
                        if nx < 0 || nx >= want_w {
                            continue;
                        }
                        let old_i = (y as usize) * (old_w as usize) + x as usize;
                        let new_i = (ny as usize) * (want_w as usize) + nx as usize;
                        self.light[new_i] = old_light[old_i];
                        self.light_coverage[new_i] = old_coverage[old_i];
                        self.discover_coverage[new_i] = old_disc_cov[old_i];
                        self.discover_light[new_i] = old_disc_light[old_i];
                    }
                }
            }
            grid_changed = true;
        }

        // Any kind's geometry changing invalidates every cached mask, not
        // only that kind's.
        let mut geometry_settings_changed = view_ownership != self.cached_view_ownership;
        for kind in 0..SOURCE_KIND_COUNT {
            if self.geometry[kind] != self.cached_geometry[kind] {
                geometry_settings_changed = true;
            }
        }

        let retain_absent_sources = frame_sources.is_empty()
            && !self.source_masks.is_empty()
            && self.empty_source_frames < EMPTY_SOURCE_GRACE_FRAMES;
        if frame_sources.is_empty() && !self.source_masks.is_empty() {
            self.empty_source_frames += 1;
        }
        if !frame_sources.is_empty() {
            self.empty_source_frames = 0;
        }

        let mut dirty_feather_regions: Vec<FeatherRegion> = Vec::new();
        self.native_union_changes.clear();
        if geometry_settings_changed {
            self.source_masks.clear();
            self.clear_light_union();
            self.far_source_masks.clear();
            self.clear_far_light_union();
        } else if grid_changed {
            for mask in &self.source_masks {
                Self::add_feather_region_mask(&mut dirty_feather_regions, mask, self.origin_cell_x, self.origin_cell_y);
            }
        }

        // Drop masks that stopped belonging to the active set (or whose worm
        // died) before adding/rebuilding the current friendly set.
        let mut i = 0usize;
        while i < self.source_masks.len() {
            if retain_absent_sources
                || frame_sources
                    .iter()
                    .any(|s| same_source_key(s, &self.source_masks[i].source))
            {
                i += 1;
                continue;
            }
            Self::add_feather_region_mask(&mut dirty_feather_regions, &self.source_masks[i], self.origin_cell_x, self.origin_cell_y);
            let mask = self.source_masks.remove(i);
            self.apply_source_mask_native(&mask, -1);
        }
        if any_hybrid_far_active {
            let mut i = 0usize;
            while i < self.far_source_masks.len() {
                if retain_absent_sources
                    || frame_sources
                        .iter()
                        .any(|s| same_source_key(s, &self.far_source_masks[i].source))
                {
                    i += 1;
                    continue;
                }
                let mask = self.far_source_masks.remove(i);
                self.apply_source_mask_far(&mask, -1);
            }
        }

        // Rebuild dirty source masks, tier by tier.
        for source in &frame_sources {
            let geom = *self.geometry_for(source.kind);
            if !geom.sees() {
                continue;
            }
            let source_circle_radius = self.circle_radius_for(source);
            // What the NEAR tier stamps. In hybrid mode the circle stops at
            // the detail boundary and the far tier carries it the rest of the
            // way; otherwise the near tier is the only tier and draws all of
            // it.
            let near_circle_radius = if geom.hybrid_far_active {
                source_circle_radius.min(geom.native_detail_radius)
            } else {
                source_circle_radius
            };
            let native_index = self
                .source_masks
                .iter()
                .position(|m| same_source_key(&m.source, source));
            let far_index = if geom.hybrid_far_active {
                self.far_source_masks
                    .iter()
                    .position(|m| same_source_key(&m.source, source))
            } else {
                None
            };
            let native_changed = match native_index {
                None => true,
                Some(idx) => {
                    let geometry_changed = if geom.hybrid_far_active {
                        !same_near_source_geometry(&self.source_masks[idx].source, source)
                    } else {
                        !same_source_geometry(&self.source_masks[idx].source, source)
                    };
                    // Terrain edits under a stationary source change no geometry,
                    // so force the re-cast when its lit bounds overlap the dirty
                    // region (or when everything is dirty).
                    geometry_changed
                        || self.terrain_dirty_all
                        || self.mask_overlaps_terrain_dirty(&self.source_masks[idx])
                }
            };
            let far_changed = geom.hybrid_far_active
                && match far_index {
                    None => true,
                    Some(idx) => {
                        !self.same_far_source_geometry(
                            &self.far_source_masks[idx].source,
                            source,
                            &geom,
                            solid,
                        ) || self.terrain_dirty_all
                            || self.mask_overlaps_terrain_dirty(&self.far_source_masks[idx])
                    }
                };

            if native_changed {
                let mut new_mask = SourceMask::new();
                std::mem::swap(&mut new_mask.cells, &mut self.native_mask_build_cells);
                let near_cone = if geom.hybrid_far_active {
                    0
                } else {
                    self.effective_cone_length_for(source, geom.cone_length)
                };
                self.rasterize_source_mask(
                    &mut new_mask,
                    source,
                    near_circle_radius,
                    geom.cone_width,
                    near_cone,
                    geom.los,
                    1,
                    0,
                    solid,
                    true,
                );
                order_source_mask_cells_from_scratch(&mut new_mask, &self.scratch);
                if geom.hybrid_far_active {
                    let probe = Self::aperture_probe_radius(&geom, source_circle_radius);
                    build_far_aperture(&mut new_mask, &self.scratch, probe, solid);
                }
                match native_index {
                    Some(idx) => {
                        self.apply_native_source_mask_delta(idx, &new_mask);
                        self.native_mask_build_cells =
                            std::mem::take(&mut self.source_masks[idx].cells);
                        self.source_masks[idx] = new_mask;
                    }
                    None => {
                        Self::add_feather_region_mask(&mut dirty_feather_regions, &new_mask, self.origin_cell_x, self.origin_cell_y);
                        self.apply_source_mask_native(&new_mask, 1);
                        self.source_masks.push(new_mask);
                    }
                }
            } else if let Some(idx) = native_index {
                self.source_masks[idx].source = source.clone();
            }

            if !geom.hybrid_far_active {
                continue;
            }
            if far_changed {
                // Both radii are measured from the near mask's real rim,
                // which is its circle -- the hybrid near cast is passed
                // cone_length 0.
                let near_rim = source_circle_radius.min(geom.native_detail_radius);
                let far_minimum_light_radius =
                    (near_rim - FAR_HANDOFF_OVERLAP_PIXELS).max(0);
                let Some(current_native_index) = self
                    .source_masks
                    .iter()
                    .position(|m| same_source_key(&m.source, source))
                else {
                    continue;
                };
                // Start occluding exactly where the aperture's rim probe
                // stops looking.
                let far_minimum_opaque_radius =
                    Self::aperture_probe_radius(&geom, source_circle_radius).max(0);
                let mut far_mask = SourceMask::new();
                self.cast_far_source_mask(
                    &mut far_mask,
                    source,
                    source_circle_radius,
                    geom.cone_width,
                    self.effective_cone_length_for(source, geom.cone_length),
                    self.far_cell_size,
                    far_minimum_light_radius,
                    far_minimum_opaque_radius,
                    current_native_index,
                    geom.los,
                    solid,
                );
                match far_index {
                    Some(idx) => {
                        self.apply_far_source_mask_delta(idx, &far_mask);
                        self.far_source_masks[idx] = far_mask;
                    }
                    None => {
                        self.apply_source_mask_far(&far_mask, 1);
                        self.far_source_masks.push(far_mask);
                    }
                }
            } else if let Some(idx) = far_index {
                self.far_source_masks[idx].source = source.clone();
            }
        }

        // Publish this frame's apertures. One source without a usable rim
        // probe makes the union unrestricted, so the whole list is dropped
        // rather than clipping the far field against a partial answer.
        self.far_apertures.clear();
        if any_hybrid_far_active {
            let mut wedge_count = 0usize;
            for mask in &self.source_masks {
                if !self.geometry_for(mask.source.kind).hybrid_far_active {
                    continue;
                }
                if !mask.aperture_valid {
                    self.far_apertures.clear();
                    break;
                }
                wedge_count += mask.aperture.wedges.len();
                if wedge_count > MAX_PUBLISHED_APERTURE_WEDGES {
                    self.far_apertures.clear();
                    break;
                }
                self.far_apertures.push(mask.aperture.clone());
            }
        }

        // Feathering is derived from the hard coverage union. Normal movement
        // rebuilds only tiles affected by real union transitions.
        self.append_changed_feather_tiles(&mut dirty_feather_regions, FEATHER_CELLS);
        self.rebuild_feather_regions(&dirty_feather_regions, FEATHER_CELLS);

        // Record terrain memory from the discovering union (worms and owned
        // animals only -- projectile light is public but transient).
        let map_w = self.map_w;
        let solid_usize = |x: usize, y: usize| solid(x as i32, y as i32);
        if self.grid_w == map_w as i32
            && self.grid_h == self.map_h as i32
            && self.origin_cell_x == 0
            && self.origin_cell_y == 0
        {
            self.discovery.record(&self.discover_light, solid_usize);
        } else {
            // The canvas grew past the map (sky/side cutouts); record only
            // the in-map window.
            let mut shifted = vec![0u8; map_w * self.map_h];
            for y in 0..self.map_h {
                let gy = y as i32 - self.origin_cell_y;
                if gy < 0 || gy >= self.grid_h {
                    continue;
                }
                for x in 0..map_w {
                    let gx = x as i32 - self.origin_cell_x;
                    if gx < 0 || gx >= self.grid_w {
                        continue;
                    }
                    shifted[y * map_w + x] =
                        self.discover_light[(gy as usize) * (self.grid_w as usize) + gx as usize];
                }
            }
            self.discovery.record(&shifted, solid_usize);
        }

        self.cached_geometry = self.geometry;
        self.cached_view_ownership = view_ownership;

        // The dirty region has been honoured by every re-cast above; a change
        // is reported once and consumed, not re-applied every frame.
        self.terrain_dirty_all = false;
        self.terrain_dirty_region = false;
    }

    /// Far shadowcasting is performed on the coarse grid, so motion within
    /// one far cell cannot change its geometry. A full-circle cone is
    /// likewise independent of aim, active state, and left/right facing.
    fn same_far_source_geometry(
        &self,
        a: &VisionSource,
        b: &VisionSource,
        geom: &SourceGeometry,
        solid: &(dyn Fn(i32, i32) -> bool + Sync),
    ) -> bool {
        let cell_size = self.far_cell_size;
        if !same_source_key(a, b)
            || pixels_to_grid_floor(a.x, cell_size) != pixels_to_grid_floor(b.x, cell_size)
            || pixels_to_grid_floor(a.y, cell_size) != pixels_to_grid_floor(b.y, cell_size)
            || solid(a.x, a.y) != solid(b.x, b.y)
        {
            return false;
        }
        let a_cone = self.effective_cone_length_for(a, geom.cone_length);
        let b_cone = self.effective_cone_length_for(b, geom.cone_length);
        if a_cone != b_cone {
            return false;
        }
        // No cone at all, or one that covers every direction: aim cannot
        // matter.
        if a_cone == 0 || geom.cone_width >= 360 {
            return true;
        }
        a.aim_angle == b.aim_angle
            && a.dir_x == b.dir_x
            && a.dir_y == b.dir_y
            && a.facing == b.facing
            && a.active == b.active
    }

    #[allow(clippy::too_many_arguments)]
    fn rasterize_source_mask(
        &mut self,
        mask: &mut SourceMask,
        source: &VisionSource,
        radius: i32,
        cone_width: i32,
        cone_length: i32,
        use_los: bool,
        cell_size: i32,
        minimum_light_radius: i32,
        solid: &(dyn Fn(i32, i32) -> bool + Sync),
        parallel: bool,
    ) {
        let extent_px = radius.max(cone_length).max(0);
        let extent_cells = pixels_to_grid_ceil(extent_px, cell_size);
        begin_source_mask(mask, &mut self.scratch, source, extent_cells, cell_size);
        let env = CastEnv {
            mask_origin_x: mask.origin_x,
            mask_origin_y: mask.origin_y,
            mask_width: mask.width,
            mask_height: mask.height,
            shape: &build_vision_shape(source, radius, cone_width, cone_length),
            cell_size,
            scratch: &self.scratch,
            solid,
            minimum_light_distance_sq: minimum_light_radius.max(0) as i64
                * minimum_light_radius.max(0) as i64,
            minimum_opaque_distance_sq: 0,
        };
        let source_cell_x = pixels_to_grid_floor(source.x, cell_size);
        let source_cell_y = pixels_to_grid_floor(source.y, cell_size);

        // No collision: light the shape directly. Neither casting backend has
        // anything to compute when nothing can occlude.
        if !use_los {
            let mut accumulator = CastAccumulator::default();
            fill_vision_shape(
                &env,
                &mut accumulator,
                source,
                radius,
                cone_width,
                cone_length,
                source_cell_x,
                source_cell_y,
            );
            finish_source_mask(mask, std::slice::from_ref(&accumulator));
            return;
        }

        let mut accumulators: [CastAccumulator; 1 + SHADOW_OCTANT_COUNT] = Default::default();
        stamp_shadow_vision_inner(
            &env,
            &mut accumulators,
            Some(&self.pool),
            source,
            radius,
            cone_width,
            cone_length,
            source_cell_x,
            source_cell_y,
            parallel,
        );
        finish_source_mask(mask, &accumulators);
    }

    /// Build one source's far mask synchronously, reading the live collision
    /// grid. `circle_radius` is the source's FULL circle, not the part the
    /// near tier kept: the far tier lights from minimum_light_radius
    /// outwards, so handing it the whole circle is what makes the annulus
    /// past the detail boundary appear at all.
    #[allow(clippy::too_many_arguments)]
    fn cast_far_source_mask(
        &mut self,
        mask: &mut SourceMask,
        source: &VisionSource,
        circle_radius: i32,
        cone_width: i32,
        cone_length: i32,
        cell_size: i32,
        minimum_light_radius: i32,
        minimum_opaque_radius: i32,
        near_index: usize,
        use_los: bool,
        solid: &(dyn Fn(i32, i32) -> bool + Sync),
    ) {
        if !use_los {
            // No collision: fill the far shape at the coarse cell size and
            // skip both the shadowcast and the angular clip against the near
            // mask. minimum_light_radius is deliberately ignored: filling
            // from the centre overlaps by the whole near circle, which is the
            // same error in the same safe direction. The masks are unioned,
            // so overlap costs a few redundant cells and a gap would be
            // visible clipped vision.
            let extent_px = cone_length.max(0);
            begin_source_mask(
                mask,
                &mut self.scratch,
                source,
                pixels_to_grid_ceil(extent_px, cell_size),
                cell_size,
            );
            let env = CastEnv {
                mask_origin_x: mask.origin_x,
                mask_origin_y: mask.origin_y,
                mask_width: mask.width,
                mask_height: mask.height,
                shape: &build_vision_shape(source, circle_radius, cone_width, cone_length),
                cell_size,
                scratch: &self.scratch,
                solid,
                minimum_light_distance_sq: 0,
                minimum_opaque_distance_sq: 0,
            };
            let mut accumulator = CastAccumulator::default();
            let source_cell_x = pixels_to_grid_floor(source.x, cell_size);
            let source_cell_y = pixels_to_grid_floor(source.y, cell_size);
            fill_vision_shape(
                &env,
                &mut accumulator,
                source,
                circle_radius,
                cone_width,
                cone_length,
                source_cell_x,
                source_cell_y,
            );
            finish_source_mask(mask, std::slice::from_ref(&accumulator));
            order_source_mask_cells_from_scratch(mask, &self.scratch);
            return;
        }
        let extent_px = circle_radius.max(cone_length).max(0);
        begin_source_mask(
            mask,
            &mut self.scratch,
            source,
            pixels_to_grid_ceil(extent_px, cell_size),
            cell_size,
        );
        let env = CastEnv {
            mask_origin_x: mask.origin_x,
            mask_origin_y: mask.origin_y,
            mask_width: mask.width,
            mask_height: mask.height,
            shape: &build_vision_shape(source, circle_radius, cone_width, cone_length),
            cell_size,
            scratch: &self.scratch,
            solid,
            minimum_light_distance_sq: minimum_light_radius.max(0) as i64
                * minimum_light_radius.max(0) as i64,
            minimum_opaque_distance_sq: minimum_opaque_radius.max(0) as i64
                * minimum_opaque_radius.max(0) as i64,
        };
        let source_cell_x = pixels_to_grid_floor(source.x, cell_size);
        let source_cell_y = pixels_to_grid_floor(source.y, cell_size);
        let mut accumulators: [CastAccumulator; 1 + SHADOW_OCTANT_COUNT] = Default::default();
        stamp_shadow_vision_inner(
            &env,
            &mut accumulators,
            Some(&self.pool),
            source,
            circle_radius,
            cone_width,
            cone_length,
            source_cell_x,
            source_cell_y,
            true,
        );
        finish_source_mask(mask, &accumulators);
        order_source_mask_cells_from_scratch(mask, &self.scratch);
        clip_far_mask_to_near_angular_visibility(
            mask,
            &self.source_masks[near_index],
            solid,
        );
    }

    // -- mask application (the source-count union) ---------------------------

    /// Add or remove one mask's cells from a light/coverage grid pair, plus
    /// the discovering pair when this kind discovers. Returns the cells whose
    /// hard-union state actually flipped (for feather invalidation).
    #[allow(clippy::too_many_arguments)]
    fn apply_source_mask_delta_to(
        old: Option<&SourceMask>,
        new: Option<&SourceMask>,
        light: &mut [u8],
        coverage: &mut [u8],
        discover_coverage: &mut [u8],
        discover_light: &mut [u8],
        grid_w: i32,
        grid_h: i32,
        grid_origin_x: i32,
        grid_origin_y: i32,
        union_changes: Option<&mut Vec<(i32, i32)>>,
    ) {
        let mut union_changes = union_changes;
        let apply_cell = &mut |mask: &SourceMask,
                              cell: &SourceCell,
                              direction: i32,
                              union_changes: &mut Option<&mut Vec<(i32, i32)>>| {
            let cx = mask.origin_x + cell.local_x as i32 - grid_origin_x;
            let cy = mask.origin_y + cell.local_y as i32 - grid_origin_y;
            if cx < 0 || cx >= grid_w || cy < 0 || cy >= grid_h {
                return;
            }
            let index = (cy as usize) * (grid_w as usize) + cx as usize;
            // The discovering plane rides the same add/remove as the light
            // one, so it stays exactly in step with the incremental mask
            // updates rather than being rebuilt.
            let discover_len = discover_coverage.len();
            if mask.source.kind.discovers() && discover_len != 0 {
                let discover = &mut discover_coverage[index];
                if direction > 0 {
                    *discover = discover.saturating_add(1);
                } else if *discover > 0 {
                    *discover -= 1;
                }
                let now = *discover;
                if discover_light.len() == discover_len {
                    discover_light[index] = if now > 0 { 255 } else { 0 };
                }
            }
            let coverage = &mut coverage[index];
            let was_lit = *coverage != 0;
            if direction > 0 {
                // A game cannot create 255 friendly worms. Saturating still
                // keeps fog conservative if a malformed source list ever did.
                *coverage = coverage.saturating_add(1);
                if *coverage > 0 {
                    light[index] = 255;
                }
            } else {
                if *coverage == 0 {
                    return;
                }
                *coverage -= 1;
                if *coverage == 0 {
                    light[index] = 0;
                }
            }
            if was_lit != (*coverage != 0) {
                if let Some(changes) = union_changes.as_deref_mut() {
                    changes.push((cx, cy));
                }
            }
        };

        let (old_sorted, new_sorted) = (
            old.map(|m| m.cells_sorted).unwrap_or(true),
            new.map(|m| m.cells_sorted).unwrap_or(true),
        );
        let same_cell_size = match (old, new) {
            (Some(o), Some(n)) => o.cell_size == n.cell_size,
            _ => true,
        };

        if !(old_sorted && new_sorted && same_cell_size) {
            if let Some(old) = old {
                for cell in &old.cells {
                    apply_cell(old, cell, -1, &mut union_changes);
                }
            }
            if let Some(new) = new {
                for cell in &new.cells {
                    apply_cell(new, cell, 1, &mut union_changes);
                }
            }
            return;
        }

        // Both sorted: merge like sets so a moved source removes only the
        // cells it actually vacated and adds only the ones it newly covers.
        let (mut old_index, mut new_index) = (0usize, 0usize);
        let (old_cells, new_cells) = (
            old.map(|m| &m.cells[..]).unwrap_or(&[]),
            new.map(|m| &m.cells[..]).unwrap_or(&[]),
        );
        while old_index < old_cells.len() && new_index < new_cells.len() {
            let old_cell = &old_cells[old_index];
            let new_cell = &new_cells[new_index];
            let old_mask = old.unwrap();
            let new_mask = new.unwrap();
            let old_y = old_mask.origin_y + old_cell.local_y as i32;
            let new_y = new_mask.origin_y + new_cell.local_y as i32;
            let old_x = old_mask.origin_x + old_cell.local_x as i32;
            let new_x = new_mask.origin_x + new_cell.local_x as i32;
            if old_y == new_y && old_x == new_x {
                old_index += 1;
                new_index += 1;
            } else if old_y < new_y || (old_y == new_y && old_x < new_x) {
                apply_cell(old_mask, old_cell, -1, &mut union_changes);
                old_index += 1;
            } else {
                apply_cell(new_mask, new_cell, 1, &mut union_changes);
                new_index += 1;
            }
        }
        if let Some(old_mask) = old {
            for cell in &old_cells[old_index..] {
                apply_cell(old_mask, cell, -1, &mut union_changes);
            }
        }
        if let Some(new_mask) = new {
            for cell in &new_cells[new_index..] {
                apply_cell(new_mask, cell, 1, &mut union_changes);
            }
        }
    }

    fn apply_source_mask_native(&mut self, mask: &SourceMask, direction: i32) {
        let (old, new) = if direction > 0 {
            (None, Some(mask))
        } else {
            (Some(mask), None)
        };
        Self::apply_source_mask_delta_to(
            old,
            new,
            &mut self.light,
            &mut self.light_coverage,
            &mut self.discover_coverage,
            &mut self.discover_light,
            self.grid_w,
            self.grid_h,
            self.origin_cell_x,
            self.origin_cell_y,
            None,
        );
    }

    fn apply_native_source_mask_delta(&mut self, old_index: usize, new_mask: &SourceMask) {
        // Split the borrow: take the old mask out, apply, put it back.
        let old_mask = std::mem::replace(&mut self.source_masks[old_index], SourceMask::new());
        let mut changes = std::mem::take(&mut self.native_union_changes);
        Self::apply_source_mask_delta_to(
            Some(&old_mask),
            Some(new_mask),
            &mut self.light,
            &mut self.light_coverage,
            &mut self.discover_coverage,
            &mut self.discover_light,
            self.grid_w,
            self.grid_h,
            self.origin_cell_x,
            self.origin_cell_y,
            Some(&mut changes),
        );
        self.native_union_changes = changes;
        self.source_masks[old_index] = old_mask;
    }

    fn apply_source_mask_far(&mut self, mask: &SourceMask, direction: i32) {
        let (old, new) = if direction > 0 {
            (None, Some(mask))
        } else {
            (Some(mask), None)
        };
        Self::apply_source_mask_delta_to(
            old,
            new,
            &mut self.far_light,
            &mut self.far_light_coverage,
            &mut self.far_discover_coverage,
            &mut self.far_discover_light,
            self.far_grid_w,
            self.far_grid_h,
            self.far_origin_cell_x,
            self.far_origin_cell_y,
            None,
        );
    }

    fn apply_far_source_mask_delta(&mut self, old_index: usize, new_mask: &SourceMask) {
        let old_mask =
            std::mem::replace(&mut self.far_source_masks[old_index], SourceMask::new());
        Self::apply_source_mask_delta_to(
            Some(&old_mask),
            Some(new_mask),
            &mut self.far_light,
            &mut self.far_light_coverage,
            &mut self.far_discover_coverage,
            &mut self.far_discover_light,
            self.far_grid_w,
            self.far_grid_h,
            self.far_origin_cell_x,
            self.far_origin_cell_y,
            None,
        );
        self.far_source_masks[old_index] = old_mask;
    }

    // -- feather -------------------------------------------------------------

    fn add_feather_region_mask(regions: &mut Vec<FeatherRegion>, mask: &SourceMask, origin_cell_x: i32, origin_cell_y: i32) {
        if mask.cells.is_empty() {
            return;
        }
        let region = FeatherRegion {
            left: pixels_to_grid_floor(mask.lit_left, 1) - origin_cell_x,
            top: pixels_to_grid_floor(mask.lit_top, 1) - origin_cell_y,
            right: pixels_to_grid_ceil(mask.lit_right, 1) - origin_cell_x,
            bottom: pixels_to_grid_ceil(mask.lit_bottom, 1) - origin_cell_y,
            exact_output: false,
        };
        Self::add_feather_region(regions, region);
    }

    fn add_feather_region(regions: &mut Vec<FeatherRegion>, mut region: FeatherRegion) {
        if is_empty_region(&region) {
            return;
        }
        let mut i = 0usize;
        while i < regions.len() {
            let other = regions[i];
            if region.left >= other.right
                || region.right <= other.left
                || region.top >= other.bottom
                || region.bottom <= other.top
            {
                i += 1;
                continue;
            }
            region.left = region.left.min(other.left);
            region.top = region.top.min(other.top);
            region.right = region.right.max(other.right);
            region.bottom = region.bottom.max(other.bottom);
            region.exact_output = region.exact_output && other.exact_output;
            regions.remove(i);
        }
        regions.push(region);
    }

    fn expand_and_clip(&self, mut region: FeatherRegion, cells: i32) -> FeatherRegion {
        let cells = cells.max(0);
        region.left = (region.left - cells).max(0);
        region.top = (region.top - cells).max(0);
        region.right = (region.right + cells).min(self.grid_w);
        region.bottom = (region.bottom + cells).min(self.grid_h);
        region
    }

    /// Convert hard-union transitions into fixed output tiles. A one-pixel
    /// worm move normally changes only the perimeter of its mask; retaining
    /// that sparsity avoids rebuilding the untouched centre of the circle.
    fn append_changed_feather_tiles(
        &mut self,
        regions: &mut Vec<FeatherRegion>,
        influence_cells: i32,
    ) {
        if self.native_union_changes.is_empty() || self.grid_w <= 0 || self.grid_h <= 0 {
            return;
        }
        let influence_cells = influence_cells.max(0);
        let tile_columns = (self.grid_w + FEATHER_TILE_SIZE - 1) >> FEATHER_TILE_SHIFT;
        let tile_rows = (self.grid_h + FEATHER_TILE_SIZE - 1) >> FEATHER_TILE_SHIFT;
        let tile_count = (tile_columns as usize) * (tile_rows as usize);
        if tile_columns != self.feather_tile_columns || tile_rows != self.feather_tile_rows {
            self.feather_tile_columns = tile_columns;
            self.feather_tile_rows = tile_rows;
            self.feather_dirty_tile_marks = vec![0; tile_count];
            self.feather_dirty_tile_generation = 1;
        } else {
            self.feather_dirty_tile_generation =
                self.feather_dirty_tile_generation.wrapping_add(1);
            if self.feather_dirty_tile_generation == 0 {
                self.feather_dirty_tile_marks.fill(0);
                self.feather_dirty_tile_generation = 1;
            }
        }
        self.feather_dirty_tile_indices.clear();

        for &(cx, cy) in &self.native_union_changes {
            let left = (cx - influence_cells).max(0);
            let top = (cy - influence_cells).max(0);
            let right = (cx + influence_cells).min(self.grid_w - 1);
            let bottom = (cy + influence_cells).min(self.grid_h - 1);
            if left > right || top > bottom {
                continue;
            }
            let first_tile_x = left >> FEATHER_TILE_SHIFT;
            let last_tile_x = right >> FEATHER_TILE_SHIFT;
            let first_tile_y = top >> FEATHER_TILE_SHIFT;
            let last_tile_y = bottom >> FEATHER_TILE_SHIFT;
            for tile_y in first_tile_y..=last_tile_y {
                for tile_x in first_tile_x..=last_tile_x {
                    let index = tile_y * tile_columns + tile_x;
                    if self.feather_dirty_tile_marks[index as usize]
                        == self.feather_dirty_tile_generation
                    {
                        continue;
                    }
                    self.feather_dirty_tile_marks[index as usize] =
                        self.feather_dirty_tile_generation;
                    self.feather_dirty_tile_indices.push(index);
                }
            }
        }

        let indices = std::mem::take(&mut self.feather_dirty_tile_indices);
        for index in &indices {
            let tile_x = index % tile_columns;
            let tile_y = index / tile_columns;
            regions.push(FeatherRegion {
                left: tile_x << FEATHER_TILE_SHIFT,
                top: tile_y << FEATHER_TILE_SHIFT,
                right: ((tile_x + 1) << FEATHER_TILE_SHIFT).min(self.grid_w),
                bottom: ((tile_y + 1) << FEATHER_TILE_SHIFT).min(self.grid_h),
                exact_output: true,
            });
        }
        self.feather_dirty_tile_indices = indices;
    }

    fn rebuild_feather_regions(&mut self, regions: &[FeatherRegion], feather_cells: i32) {
        if self.light.is_empty() || self.light_coverage.is_empty() {
            return;
        }
        let feather_cells = feather_cells.clamp(0, 2);
        for region in regions {
            self.rebuild_feather_region(region, feather_cells);
        }
    }

    /// Feathering is a 3x3/5x5 square dilation of the hard union. The binary
    /// input is packed eight cells per byte: two horizontal one-bit
    /// dilations, then ORing nearby rows, produce the exact same Chebyshev
    /// rings with far less memory work than a byte distance field.
    fn rebuild_feather_region(&mut self, source_bounds: &FeatherRegion, feather_cells: i32) {
        let output = if source_bounds.exact_output {
            *source_bounds
        } else {
            self.expand_and_clip(*source_bounds, feather_cells)
        };
        if is_empty_region(&output) {
            return;
        }

        let input = self.expand_and_clip(output, feather_cells);
        let input_w = input.right - input.left;
        let input_h = input.bottom - input.top;
        let byte_stride = ((input_w + 7) >> 3) as usize;
        let byte_count = byte_stride * input_h as usize;
        if self.feather_hard_bits.len() != byte_count {
            self.feather_hard_bits.resize(byte_count, 0);
            self.feather_near_bits.resize(byte_count, 0);
            self.feather_far_bits.resize(byte_count, 0);
        }

        let grid_w = self.grid_w as usize;
        for y in 0..input_h {
            let source_row = ((input.top + y) as usize) * grid_w + input.left as usize;
            let bit_row = (y as usize) * byte_stride;
            for byte in 0..byte_stride {
                let first_cell = (byte << 3) as i32;
                let coverage = &self.light_coverage[source_row + first_cell as usize..];
                let bits: u8 = if first_cell + 8 <= input_w {
                    (coverage[0] != 0) as u8
                        | ((coverage[1] != 0) as u8) << 1
                        | ((coverage[2] != 0) as u8) << 2
                        | ((coverage[3] != 0) as u8) << 3
                        | ((coverage[4] != 0) as u8) << 4
                        | ((coverage[5] != 0) as u8) << 5
                        | ((coverage[6] != 0) as u8) << 6
                        | ((coverage[7] != 0) as u8) << 7
                } else {
                    let mut b = 0u8;
                    let mut bit = 0;
                    while first_cell + bit < input_w {
                        if coverage[bit as usize] != 0 {
                            b |= 1 << bit;
                        }
                        bit += 1;
                    }
                    b
                };
                self.feather_hard_bits[bit_row + byte] = bits;
            }
        }

        fn dilate_horizontal(source: &[u8], destination: &mut [u8], byte_stride: usize, input_h: i32) {
            for y in 0..input_h as usize {
                let row = y * byte_stride;
                for byte in 0..byte_stride {
                    let current = source[row + byte];
                    let mut result = current;
                    result |= current << 1;
                    result |= current >> 1;
                    if byte > 0 {
                        result |= source[row + byte - 1] >> 7;
                    }
                    if byte + 1 < byte_stride {
                        result |= source[row + byte + 1] << 7;
                    }
                    destination[row + byte] = result;
                }
            }
        }

        let output_left = output.left - input.left;
        let output_right = output.right - input.left;
        if feather_cells == 0 {
            // Restore the exact hard union within every cell that could have
            // gained or lost a previous halo.
            for y in output.top..output.bottom {
                let row = (y as usize) * grid_w;
                for x in output.left..output.right {
                    self.light[row + x as usize] =
                        if self.light_coverage[row + x as usize] != 0 { 255 } else { 0 };
                }
            }
            return;
        }

        dilate_horizontal(&self.feather_hard_bits.clone(), &mut self.feather_near_bits, byte_stride, input_h);
        if feather_cells == 2 {
            dilate_horizontal(&self.feather_near_bits.clone(), &mut self.feather_far_bits, byte_stride, input_h);
        }
        for y in output.top..output.bottom {
            let output_row = (y as usize) * grid_w;
            let local_y = y - input.top;
            for byte in 0..byte_stride {
                let bit_index = (local_y as usize) * byte_stride + byte;
                let hard = self.feather_hard_bits[bit_index];
                let mut near = self.feather_near_bits[bit_index];
                if local_y > 0 {
                    near |= self.feather_near_bits[bit_index - byte_stride];
                }
                if local_y + 1 < input_h {
                    near |= self.feather_near_bits[bit_index + byte_stride];
                }
                let mut far = 0u8;
                if feather_cells == 2 {
                    far = self.feather_far_bits[bit_index];
                    for row_offset in -2..=2 {
                        let nearby_y = local_y + row_offset;
                        if nearby_y >= 0 && nearby_y < input_h {
                            far |= self.feather_far_bits
                                [(nearby_y as usize) * byte_stride + byte];
                        }
                    }
                }
                let local_x = (byte << 3) as i32;
                for bit in 0..8 {
                    let pixel_x = local_x + bit;
                    if pixel_x < output_left || pixel_x >= output_right {
                        continue;
                    }
                    let flag = 1u8 << bit;
                    let value = if hard & flag != 0 {
                        255
                    } else if near & flag != 0 {
                        if feather_cells == 1 {
                            OUTER_LIGHT
                        } else {
                            INNER_LIGHT
                        }
                    } else if far & flag != 0 {
                        OUTER_LIGHT
                    } else {
                        0
                    };
                    self.light[output_row + (input.left + pixel_x) as usize] = value;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Value;

    fn all_open(_x: i32, _y: i32) -> bool {
        false
    }

    fn scheme_with(overrides: &[(&str, Value)]) -> Scheme {
        let mut s = Scheme::default();
        for (name, value) in overrides {
            s.set(name, value).unwrap();
        }
        s
    }

    /// A worm source at (x, y) with the given facing, active and aimable.
    fn worm_at(x: i32, y: i32) -> VisionSource {
        VisionSource {
            kind: SourceKind::Worm,
            team_number: 1,
            worm_number: 1,
            x,
            y,
            facing: 1,
            active: false,
            aimable: true,
            circle_radius: -1,
            ..Default::default()
        }
    }

    #[test]
    fn wall_blocks_los() {
        // 60x40 map, open except a vertical wall at x=30 from y=0..40.
        // Worm at (20, 20) with a 25px circle: cells before the wall are lit
        // hard, and cells past the wall are in shadow. The 2-cell feather is
        // a cosmetic ramp painted PAST the true vision boundary, so the first
        // couple of shadowed cells may still read 192/96; the hard union and
        // everything past the feather must be dark.
        let scheme = scheme_with(&[("circle_radius", Value::Int(25))]);
        let mut fog = Fog::new(60, 40, scheme);
        let wall = |x: i32, _y: i32| x == 30;
        fog.update(&[worm_at(20, 20)], wall);
        assert_eq!(fog.light_at(20, 20), 255, "worm cell lit");
        assert_eq!(fog.light_at(25, 20), 255, "open cell before wall lit");
        assert_eq!(fog.light_at(30, 20), 255, "wall face itself is lit");
        // The wall blocks the hard union: nothing past it is hard-lit.
        assert!(fog.light_at(31, 20) < 255, "cell behind wall not hard-lit");
        assert_eq!(fog.light_at(34, 20), 0, "past the feather: full shadow");
        assert_eq!(fog.light_at(40, 20), 0, "far cell behind wall is dark");
    }

    #[test]
    fn cone_clips_to_wedge() {
        // Worm facing right with a 45-degree cone of length 100 and no
        // circle. Cells along the cone axis are lit; cells perpendicular to
        // it (outside the wedge) are dark.
        let scheme = scheme_with(&[
            ("circle_radius", Value::Int(0)),
            ("cone_length", Value::Int(100)),
            ("cone_width", Value::Int(45)),
        ]);
        let mut fog = Fog::new(200, 200, scheme);
        let mut source = worm_at(100, 100);
        source.active = true;
        source.dir_x = 1024;
        source.dir_y = 0;
        fog.update(&[source], all_open);
        assert_eq!(fog.light_at(150, 100), 255, "along cone axis");
        assert_eq!(fog.light_at(100, 150), 0, "perpendicular: outside wedge");
        assert_eq!(fog.light_at(50, 100), 0, "behind: outside wedge");
        // Inside the wedge: 45 degrees around +x, so (140, 110) is within
        // ~22 degrees of the axis.
        assert_eq!(fog.light_at(140, 110), 255, "inside wedge");
    }

    #[test]
    fn los_false_fills_full_circle_regardless_of_terrain() {
        // Solid wall straight through the circle; with los off the far side
        // is lit anyway.
        let scheme = scheme_with(&[
            ("circle_radius", Value::Int(25)),
            ("los", Value::Bool(false)),
        ]);
        let mut fog = Fog::new(60, 40, scheme);
        let wall = |x: i32, _y: i32| x == 30;
        fog.update(&[worm_at(20, 20)], wall);
        assert_eq!(fog.light_at(31, 20), 255, "los off: wall does not block");
        assert_eq!(fog.light_at(40, 20), 255, "los off: full circle lit");
        // But the circle boundary still applies.
        assert_eq!(fog.light_at(50, 20), 0, "outside circle stays dark");
    }

    #[test]
    fn parallel_cast_matches_sequential() {
        // Build the same mask twice: once through the pool, once sequential.
        // The masks must be identical cell for cell.
        let scheme = scheme_with(&[("circle_radius", Value::Int(120))]);
        let source = worm_at(200, 200);
        // Terrain: a scattered deterministic pattern plus a wall.
        let solid = |x: i32, y: i32| {
            x == 240 || ((x.wrapping_mul(31).wrapping_add(y.wrapping_mul(17))) % 23 == 0 && (x - 200).abs() + (y - 200).abs() > 20)
        };

        let build = |parallel: bool| {
            let mut fog = Fog::new(400, 400, scheme.clone());
            let mut mask = SourceMask::new();
            let geom = *fog.geometry_for(SourceKind::Worm);
            fog.rasterize_source_mask(
                &mut mask,
                &source,
                geom.circle_radius,
                geom.cone_width,
                0,
                geom.los,
                1,
                0,
                &solid,
                parallel,
            );
            order_source_mask_cells_from_scratch(&mut mask, &fog.scratch);
            mask
        };

        let par = build(true);
        let seq = build(false);
        assert_eq!(par.cells.len(), seq.cells.len(), "same lit cell count");
        assert_eq!(
            (par.lit_left, par.lit_top, par.lit_right, par.lit_bottom),
            (seq.lit_left, seq.lit_top, seq.lit_right, seq.lit_bottom),
            "same lit bounds"
        );
        for (a, b) in par.cells.iter().zip(seq.cells.iter()) {
            assert_eq!((a.local_x, a.local_y), (b.local_x, b.local_y));
        }
    }

    #[test]
    fn feather_ring_is_192_then_96_around_255_core() {
        // A worm with a small circle on open ground: the hard union is 255,
        // ring 1 is 192, ring 2 is 96, and beyond that 0.
        let scheme = scheme_with(&[("circle_radius", Value::Int(10))]);
        let mut fog = Fog::new(60, 60, scheme);
        fog.update(&[worm_at(30, 30)], all_open);
        // The rim of the r=10 circle along +x is at x=40 (lit hard).
        assert_eq!(fog.light_at(40, 30), 255, "hard rim");
        assert_eq!(fog.light_at(41, 30), INNER_LIGHT, "first feather ring");
        assert_eq!(fog.light_at(42, 30), OUTER_LIGHT, "second feather ring");
        assert_eq!(fog.light_at(43, 30), 0, "past the feather");
    }

    #[test]
    fn discovery_floor_lifts_remembered_pixels() {
        // discover=1 (texture): seen terrain is re-shown dimmed after the
        // worm moves away. Only SOLID terrain is remembered, so the cell
        // under test must be ground the worm lit, not open air.
        let scheme = scheme_with(&[
            ("circle_radius", Value::Int(10)),
            ("discover", Value::Int(1)),
        ]);
        let mut fog = Fog::new(60, 60, scheme);
        // Open pocket around the worm; the pocket's wall at x=36 is solid
        // ground the worm's circle lights.
        let pocket = |x: i32, y: i32| {
            !((25..=35).contains(&x) && (25..=35).contains(&y))
        };
        fog.update(&[worm_at(30, 30)], pocket);
        // (36, 30) is the pocket's solid wall, lit hard by the worm.
        assert_eq!(fog.light_at(36, 30), 255, "wall face lit");
        assert!(fog.discovery().is_remembered(36, 30), "solid wall remembered");
        // Move the worm far away; the wall drops out of the live union but
        // the discovery floor keeps it lifted.
        fog.update(&[worm_at(55, 55)], pocket);
        let lifted = fog.light_at(36, 30);
        assert!(lifted > 0, "remembered terrain stays lifted, got {lifted}");
        assert!(lifted < 96, "discovery floor is dimmer than the feather");
    }

    #[test]
    fn moved_worm_recasts_only_its_own_mask() {
        // Two worms; after one moves, cells still covered by the other stay
        // lit (the source-count union does not erase shared cells).
        let scheme = scheme_with(&[("circle_radius", Value::Int(15))]);
        let mut fog = Fog::new(100, 60, scheme);
        let w1 = worm_at(30, 30);
        let mut w2 = worm_at(40, 30);
        w2.worm_number = 2;
        fog.update(&[w1.clone(), w2.clone()], all_open);
        assert_eq!(fog.light_at(35, 30), 255, "overlap lit by both");
        // Move w2 away; the overlap cell is still covered by w1 alone.
        let mut w2_moved = w2.clone();
        w2_moved.x = 80;
        fog.update(&[w1, w2_moved], all_open);
        assert_eq!(fog.light_at(35, 30), 255, "stationary worm still covers it");
        assert_eq!(fog.light_at(80, 30), 255, "moved worm lights its new cell");
    }

    #[test]
    fn explosion_lights_then_fades() {
        let scheme = scheme_with(&[("circle_radius", Value::Int(0))]);
        let mut fog = Fog::new(100, 100, scheme);
        fog.notify_explosion(50, 50, 10);
        fog.update(&[], all_open);
        assert_eq!(fog.light_at(50, 50), 255, "blast centre lit");
        assert!(fog.light_at(50 + 40, 50) > 0, "pad around crater lit");
    }

    #[test]
    fn two_tier_far_field_extends_past_native_radius() {
        // Circle radius 400 > NATIVE_DETAIL_RADIUS 250: the annulus from
        // 250 to 400 must still be lit, via the coarse far tier.
        let scheme = scheme_with(&[("circle_radius", Value::Int(400))]);
        let mut fog = Fog::new(900, 900, scheme);
        fog.update(&[worm_at(450, 450)], all_open);
        assert_eq!(fog.light_at(450 + 200, 450), 255, "inside native tier");
        assert_eq!(
            fog.light_at(450 + 300, 450),
            255,
            "past native radius, lit by far tier"
        );
        assert_eq!(fog.light_at(450 + 410, 450), 0, "past full reach");
    }

    #[test]
    fn far_tier_wall_shadow_continues_across_boundary() {
        // A wall inside the native radius must keep shadowing into the far
        // tier: the far-aperture clip keeps the shadow edge continuous.
        let scheme = scheme_with(&[("circle_radius", Value::Int(400))]);
        let mut fog = Fog::new(900, 900, scheme);
        // Vertical wall at x=550, 100px from the worm (inside the 250px
        // native tier), full height.
        let wall = |x: i32, _y: i32| x == 550;
        fog.update(&[worm_at(450, 450)], wall);
        assert_eq!(fog.light_at(549, 450), 255, "before the wall");
        // The wall blocks the hard union past it; the 2-cell feather may
        // still paint 192/96 for a couple of cells, but nothing is hard-lit.
        assert!(fog.light_at(551, 450) < 255, "just past the wall not hard-lit");
        assert_eq!(fog.light_at(554, 450), 0, "past the feather: full shadow");
        assert_eq!(
            fog.light_at(700, 450),
            0,
            "far past the wall (far tier): shadow continues"
        );
        // But the far tier still lights open directions past 250px.
        assert_eq!(fog.light_at(450, 450 + 300), 255, "open far tier lit");
    }

    #[test]
    fn terrain_change_recasts_overlapping_stationary_mask() {
        // A stationary worm's mask is cached by geometry, so a wall going up
        // inside its circle changes nothing the geometry comparison can see.
        // notify_terrain_changed must force the re-cast.
        let scheme = scheme_with(&[("circle_radius", Value::Int(25))]);
        let mut fog = Fog::new(60, 40, scheme);
        let worm = worm_at(20, 20);
        fog.update(&[worm.clone()], all_open);
        assert_eq!(fog.light_at(40, 20), 255, "open before the wall goes up");

        // Raise a wall at x=30 and tell the fog only the wall's column changed.
        let wall = |x: i32, _y: i32| x == 30;
        fog.notify_terrain_changed(30, 0, 31, 40);
        fog.update(&[worm], wall);
        assert_eq!(fog.light_at(30, 20), 255, "wall face lit");
        assert_eq!(fog.light_at(40, 20), 0, "wall now blocks: shadow behind it");
    }

    #[test]
    fn terrain_change_outside_mask_leaves_it_cached() {
        // A change far from the worm must not force a re-cast: the mask is
        // still valid. This is the steady-state cost guard, and it must not
        // regress to re-casting everything on every edit.
        let scheme = scheme_with(&[("circle_radius", Value::Int(15))]);
        let mut fog = Fog::new(100, 60, scheme);
        let worm = worm_at(20, 30);
        fog.update(&[worm.clone()], all_open);
        assert_eq!(fog.light_at(30, 30), 255);
        // Dirty a region well outside the 15px circle; the worm keeps its mask
        // and the same open cell stays lit (no spurious shadow appears).
        fog.notify_terrain_changed(80, 0, 99, 60);
        fog.update(&[worm], all_open);
        assert_eq!(fog.light_at(30, 30), 255, "unchanged mask still correct");
    }
}
