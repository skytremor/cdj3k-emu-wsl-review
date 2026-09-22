//! MainLcdStream - reads the main LCD from the QEMU shm display backend.
//!
//! Shm file layout (written by qemu/patch/shm-display.c):
//!
//!   [0]   u32  magic       0x514D5348  ("QMS\x00")
//!   [4]   u32  generation  publication counter (protocol depends on mode)
//!   [8]   u32  width
//!   [12]  u32  height
//!   [16]  u32  stride      bytes per row
//!   [20]  u32  format      1 = RGBA8888  (QEMU converts from XRGB on its side)
//!   [24]  u32  dirty_x
//!   [28]  u32  dirty_y
//!   [32]  u32  dirty_w
//!   [36]  u32  dirty_h
//!   [64]  u8[] pixels      stride × height bytes
//!
//! The original macOS writer increments `generation` once after each completed
//! blit. The WSL writer uses odd values while writing and even values for stable
//! frames. [`MainDisplayMode`] keeps those contracts explicit so WSL's stronger
//! snapshot protocol does not change the established macOS path.
//!
//! Pixel format: format=1 (RGBA8888, R,G,B,A byte order).
//! shm_gfx_update converts XRGB8888→RGBA8888 on the QEMU side so the host
//! can bulk-copy rows without any per-pixel channel swap.
//!
//! File path: `{socket_dir}/main.shm`  (created by QEMU at boot).

use memmap2::Mmap;
use std::fs::File;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub const LCD_W: usize = 1280;
pub const LCD_H: usize = 720;

const SHM_MAGIC: u32 = 0x514D_5348;
const SHM_FORMAT_RGBA8888: u32 = 1;
/// Byte offset where pixel data begins in the shm file (public for the GL upload path).
pub const SHM_PIXELS_OFFSET: usize = 64;

/// Poll interval for the shm generation counter. 500 µs comfortably tracks QEMU's
/// ~60 Hz dirty publishes without burning CPU.
const POLL_INTERVAL: Duration = Duration::from_micros(500);
/// Backoff between "shm not yet present" / "magic gone" retries.
const RECONNECT_DELAY: Duration = Duration::from_secs(1);

/// Publication and ownership policy for the main-display shared memory.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum MainDisplayMode {
    /// Original macOS contract: every changed generation is published and the
    /// UI uploads directly from the live shared-memory mapping.
    #[default]
    LegacyPublishedGeneration,
    /// WSL contract: odd means writing; an unchanged, nonzero even generation
    /// is copied into owned memory before publication.
    SequencedStableSnapshot,
}

pub const DEFAULT_MAIN_DISPLAY_MODE: MainDisplayMode = MainDisplayMode::LegacyPublishedGeneration;

/// Pixel storage carried by a dirty-region notification.
pub enum DisplayPayload {
    /// Live QEMU mapping used by the legacy zero-copy upload path.
    Mapped(Arc<Mmap>),
    /// Stable, tightly packed RGBA rows used by the sequenced WSL path.
    Owned(Vec<u8>),
}

/// A dirty-region notification with mode-specific pixel ownership.
pub struct DisplayDirty {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    /// Row stride in bytes. Mapped payloads retain the surface stride; owned
    /// payloads use the tightly packed `w * 4` stride.
    pub stride: u32,
    pub payload: DisplayPayload,
}

/// Background-thread shm reader for the main LCD.
pub struct MainLcdStream {
    slot: Arc<Mutex<Option<DisplayDirty>>>,
    connected: Arc<AtomicBool>,
    /// Monotonic count of generations observed since process start. Lets the UI
    /// gate its "still booting" overlay on actual frame production rather than
    /// just the shm being mapped.
    frames_seen: Arc<AtomicU32>,
    shm_path: String,
}

impl MainLcdStream {
    pub fn new(socket_dir: &str, gate: crate::RepaintGate) -> Self {
        Self::new_with_mode_and_control_gate(socket_dir, gate, DEFAULT_MAIN_DISPLAY_MODE, None)
    }

    pub fn new_with_control_gate(
        socket_dir: &str,
        gate: crate::RepaintGate,
        control_gate: Option<Arc<crate::ControlConnectionGate>>,
    ) -> Self {
        Self::new_with_mode_and_control_gate(
            socket_dir,
            gate,
            DEFAULT_MAIN_DISPLAY_MODE,
            control_gate,
        )
    }

    pub fn new_with_mode(
        socket_dir: &str,
        gate: crate::RepaintGate,
        mode: MainDisplayMode,
    ) -> Self {
        Self::new_with_mode_and_control_gate(socket_dir, gate, mode, None)
    }

    pub fn new_with_mode_and_control_gate(
        socket_dir: &str,
        gate: crate::RepaintGate,
        mode: MainDisplayMode,
        control_gate: Option<Arc<crate::ControlConnectionGate>>,
    ) -> Self {
        let shm_path = format!("{}/main.shm", socket_dir.trim_end_matches('/'));
        let slot: Arc<Mutex<Option<DisplayDirty>>> = Arc::new(Mutex::new(None));
        let slot_clone = Arc::clone(&slot);
        let connected = Arc::new(AtomicBool::new(false));
        let connected_clone = Arc::clone(&connected);
        let frames_seen = Arc::new(AtomicU32::new(0));
        let frames_seen_clone = Arc::clone(&frames_seen);
        let path = shm_path.clone();

        thread::Builder::new()
            .name("main-lcd-shm".into())
            .spawn(move || {
                shm_loop(
                    &path,
                    slot_clone,
                    connected_clone,
                    frames_seen_clone,
                    gate,
                    mode,
                    control_gate,
                )
            })
            .expect("spawn main-lcd-shm thread");

        Self {
            slot,
            connected,
            frames_seen,
            shm_path,
        }
    }

    /// Total generations observed by the background reader since process start.
    pub fn frames_seen(&self) -> u32 {
        self.frames_seen.load(Ordering::Relaxed)
    }

    /// Take the latest dirty notification, if any.
    pub fn take(&self) -> Option<DisplayDirty> {
        self.slot.lock().ok()?.take()
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed)
    }

    pub fn addr_str(&self) -> &str {
        &self.shm_path
    }
}

// ---------------------------------------------------------------------------
// Background thread
// ---------------------------------------------------------------------------

fn shm_loop(
    shm_path: &str,
    slot: Arc<Mutex<Option<DisplayDirty>>>,
    connected: Arc<AtomicBool>,
    frames_seen: Arc<AtomicU32>,
    gate: crate::RepaintGate,
    mode: MainDisplayMode,
    control_gate: Option<Arc<crate::ControlConnectionGate>>,
) {
    let mut wait_logged = false;
    loop {
        // Wait for the shm file to appear and contain a valid header.
        let (mmap, launch_epoch) = loop {
            let candidate_epoch = control_gate.as_ref().map(|gate| gate.current_epoch());
            match open_shm(shm_path) {
                Some(m)
                    if control_gate.as_ref().map(|gate| gate.current_epoch())
                        != candidate_epoch =>
                {
                    drop(m);
                }
                Some(m) => {
                    eprintln!("[main_stream] opened {shm_path}");
                    wait_logged = false;
                    connected.store(true, Ordering::Relaxed);
                    gate.request();
                    break (m, candidate_epoch);
                }
                None => {
                    if !wait_logged {
                        eprintln!("[main_stream] waiting for {shm_path}");
                        wait_logged = true;
                    }
                    thread::sleep(RECONNECT_DELAY);
                }
            }
        };

        poll_loop(
            &mmap,
            &slot,
            &frames_seen,
            &gate,
            mode,
            control_gate.as_ref(),
            launch_epoch,
        );

        // QEMU restarted (magic gone).
        eprintln!("[main_stream] disconnected, reconnecting");
        connected.store(false, Ordering::Relaxed);
        gate.request();
        thread::sleep(RECONNECT_DELAY);
    }
}

/// Inner loop: poll generation until magic disappears (QEMU gone/restarted).
/// A static display keeps the same generation indefinitely - that is normal,
/// not stale - so there is no timeout-based exit.
fn poll_loop(
    mmap: &Arc<Mmap>,
    slot: &Arc<Mutex<Option<DisplayDirty>>>,
    frames_seen: &Arc<AtomicU32>,
    gate: &crate::RepaintGate,
    mode: MainDisplayMode,
    control_gate: Option<&Arc<crate::ControlConnectionGate>>,
    launch_epoch: Option<crate::LaunchEpoch>,
) {
    let mut generations = GenerationTracker::new(mode, read_u32(mmap, 4));

    // Local dirty rect accumulator (x0, y0, x1, y1).
    // Accumulates the union of all dirty rects received since the last
    // successful publish.  Prevents cursor-ghost artifacts when the UI
    // thread is busy and cdj3k-emu misses intermediate dirty rect updates
    // (e.g. "erase old cursor" fires between two polls - without
    // accumulation the stale cursor pixels would never be uploaded).
    let mut acc: Option<(usize, usize, usize, usize)> = None;
    // Track surface dimensions to detect switches (640×480 → 1280×720).
    let mut last_w: usize = 0;
    let mut last_h: usize = 0;
    let mut first_frame = true;

    loop {
        thread::sleep(POLL_INTERVAL);

        // Magic check on every tick - disappears when QEMU exits or restarts.
        if read_u32(mmap, 0) != SHM_MAGIC {
            eprintln!("[main_stream] magic gone, reconnecting");
            return;
        }

        // Acquire load of generation - pairs with QEMU's RELEASE stores.
        let gen = read_u32_acquire(mmap, 4);

        let Some(generation_event) = generations.observe(gen) else {
            continue;
        };
        puffin::profile_scope!("main_lcd_gen_bump");
        if generation_event.count_immediately {
            // Preserve upstream accounting: a legacy publication is counted
            // when its generation changes, even if the UI slot is occupied.
            frames_seen.fetch_add(1, Ordering::Relaxed);
        }
        // Re-read dimensions on every frame - the surface can switch
        // mid-session (e.g. initial 640×480 QEMU console → 1280×720 Xorg).
        let width = read_u32(mmap, 8) as usize;
        let height = read_u32(mmap, 12) as usize;
        let stride = read_u32(mmap, 16) as usize;
        let format = read_u32(mmap, 20);

        if !valid_frame(mode, width, height, stride, format, mmap.len()) {
            continue;
        }

        // Reset accumulator on surface dimension change.
        let surface_changed = width != last_w || height != last_h;
        if surface_changed {
            acc = None;
            last_w = width;
            last_h = height;
        }

        // Read and validate dirty rect from header.
        let dx = read_u32(mmap, 24) as usize;
        let dy = read_u32(mmap, 28) as usize;
        let dw = read_u32(mmap, 32) as usize;
        let dh = read_u32(mmap, 36) as usize;

        let dirty_valid = dw != 0
            && dh != 0
            && dx.checked_add(dw).is_some_and(|end| end <= width)
            && dy.checked_add(dh).is_some_and(|end| end <= height);
        let force_full_frame =
            mode == MainDisplayMode::SequencedStableSnapshot && (first_frame || surface_changed);
        if !dirty_valid && !force_full_frame {
            continue;
        }

        let (dx, dy, dw, dh) = if force_full_frame {
            (0, 0, width, height)
        } else {
            (dx, dy, dw, dh)
        };

        // Expand the local accumulator to cover this dirty rect.
        acc = accumulate_dirty(acc, dx, dy, dw, dh);

        // Try to publish the accumulated region.  If the slot is still
        // occupied the accumulator keeps growing - next tick will cover
        // everything that was missed.
        if let Ok(mut g) = slot.try_lock() {
            if g.is_none() {
                if let Some((x0, y0, x1, y1)) = acc.take() {
                    let uw = x1 - x0;
                    let uh = y1 - y0;

                    if let Some(dirty) =
                        build_display_dirty(mmap, mode, gen, x0, y0, uw, uh, stride, width, height)
                    {
                        *g = Some(dirty);
                        if let (Some(control_gate), Some(epoch)) = (control_gate, launch_epoch) {
                            control_gate.observe_main(epoch, gen, width as u32, height as u32);
                        }
                        if !generation_event.count_immediately {
                            frames_seen.fetch_add(1, Ordering::Relaxed);
                        }
                        first_frame = false;
                        gate.request();
                    } else {
                        acc = Some((x0, y0, x1, y1));
                    }
                }
            }
            // Slot busy: keep accumulating, don't take() so acc remains set.
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GenerationEvent {
    count_immediately: bool,
}

struct GenerationTracker {
    mode: MainDisplayMode,
    last: u32,
}

impl GenerationTracker {
    fn new(mode: MainDisplayMode, initial: u32) -> Self {
        let last = match mode {
            // Upstream waited for the next generation after attaching.
            MainDisplayMode::LegacyPublishedGeneration => initial,
            // WSL may have published its only full frame before the reader
            // maps the file, so consume an already-stable initial generation.
            MainDisplayMode::SequencedStableSnapshot => initial.wrapping_sub(1),
        };
        Self { mode, last }
    }

    fn observe(&mut self, generation: u32) -> Option<GenerationEvent> {
        if generation == self.last {
            return None;
        }
        if self.mode == MainDisplayMode::SequencedStableSnapshot
            && !generation_is_stable(generation, generation)
        {
            return None;
        }
        self.last = generation;
        Some(GenerationEvent {
            count_immediately: self.mode == MainDisplayMode::LegacyPublishedGeneration,
        })
    }
}

fn build_display_dirty(
    mmap: &Arc<Mmap>,
    mode: MainDisplayMode,
    generation: u32,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    stride: usize,
    surface_width: usize,
    surface_height: usize,
) -> Option<DisplayDirty> {
    let (payload, published_stride) = match mode {
        MainDisplayMode::LegacyPublishedGeneration => {
            (DisplayPayload::Mapped(Arc::clone(mmap)), stride)
        }
        MainDisplayMode::SequencedStableSnapshot => (
            DisplayPayload::Owned(copy_stable_rect(
                mmap,
                generation,
                x,
                y,
                width,
                height,
                stride,
                surface_width,
                surface_height,
            )?),
            width.checked_mul(4)?,
        ),
    };
    Some(DisplayDirty {
        x: x.try_into().ok()?,
        y: y.try_into().ok()?,
        w: width.try_into().ok()?,
        h: height.try_into().ok()?,
        stride: published_stride.try_into().ok()?,
        payload,
    })
}

fn accumulate_dirty(
    acc: Option<(usize, usize, usize, usize)>,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
) -> Option<(usize, usize, usize, usize)> {
    let x1 = x.checked_add(width)?;
    let y1 = y.checked_add(height)?;
    Some(match acc {
        None => (x, y, x1, y1),
        Some((ax0, ay0, ax1, ay1)) => (ax0.min(x), ay0.min(y), ax1.max(x1), ay1.max(y1)),
    })
}

fn valid_frame(
    mode: MainDisplayMode,
    width: usize,
    height: usize,
    stride: usize,
    format: u32,
    map_len: usize,
) -> bool {
    let Some(row_bytes) = width.checked_mul(4) else {
        return false;
    };
    let Some(pixel_bytes) = stride.checked_mul(height) else {
        return false;
    };
    let Some(frame_end) = SHM_PIXELS_OFFSET.checked_add(pixel_bytes) else {
        return false;
    };
    width != 0
        && height != 0
        // The upstream reader did not gate legacy publications on the format
        // word. Retain that behavior; the new sequenced path validates its
        // owned RGBA snapshot explicitly.
        && (mode == MainDisplayMode::LegacyPublishedGeneration
            || format == SHM_FORMAT_RGBA8888)
        && stride >= row_bytes
        && frame_end <= map_len
}

fn copy_stable_rect(
    mmap: &Mmap,
    generation: u32,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    stride: usize,
    surface_width: usize,
    surface_height: usize,
) -> Option<Vec<u8>> {
    if width == 0
        || height == 0
        || x.checked_add(width)? > surface_width
        || y.checked_add(height)? > surface_height
    {
        return None;
    }
    let row_bytes = width.checked_mul(4)?;
    let total_bytes = row_bytes.checked_mul(height)?;
    let mut pixels = vec![0u8; total_bytes];
    for row in 0..height {
        let source_row = y.checked_add(row)?;
        let source_offset = SHM_PIXELS_OFFSET
            .checked_add(source_row.checked_mul(stride)?)?
            .checked_add(x.checked_mul(4)?)?;
        let source_end = source_offset.checked_add(row_bytes)?;
        if source_end > mmap.len() {
            return None;
        }
        let target_offset = row.checked_mul(row_bytes)?;
        pixels[target_offset..target_offset + row_bytes]
            .copy_from_slice(&mmap[source_offset..source_end]);
    }
    if !generation_is_stable(generation, read_u32_acquire(mmap, 4)) {
        return None;
    }
    Some(pixels)
}

fn generation_is_stable(expected: u32, observed: u32) -> bool {
    expected != 0 && expected & 1 == 0 && observed == expected
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn open_shm(path: &str) -> Option<Arc<Mmap>> {
    let file = File::open(path).ok()?;
    let mmap = unsafe { Mmap::map(&file).ok()? };
    if mmap.len() < SHM_PIXELS_OFFSET {
        return None;
    }
    if read_u32(&mmap, 0) != SHM_MAGIC {
        return None;
    }
    Some(Arc::new(mmap))
}

/// Plain little-endian read - use for non-generation fields after the acquire.
fn read_u32(mmap: &Mmap, offset: usize) -> u32 {
    let bytes: [u8; 4] = mmap[offset..offset + 4].try_into().unwrap();
    u32::from_le_bytes(bytes)
}

/// Acquire load of a u32 - pairs with QEMU's __ATOMIC_RELEASE store.
fn read_u32_acquire(mmap: &Mmap, offset: usize) -> u32 {
    // SAFETY: `mmap.as_ptr()` is page-aligned (mmap-allocated regions
    // always are), and the header layout fixes the generation counter
    // at offset 4, which is 4-byte aligned and therefore satisfies
    // `AtomicU32`'s alignment.  The offset is well within bounds of the
    // mapped region (caller-enforced via the `Mmap` size check at
    // construction time).
    let ptr = unsafe { mmap.as_ptr().add(offset) as *const AtomicU32 };
    // SAFETY: `ptr` was just derived from a live `Mmap` borrowed for the
    // duration of this call, the dereferenced `AtomicU32` provides its
    // own synchronisation, and the QEMU side performs only atomic
    // accesses to the same word.
    unsafe { (*ptr).load(Ordering::Acquire) }
}

#[cfg(test)]
mod tests {
    use super::{
        accumulate_dirty, build_display_dirty, copy_stable_rect, generation_is_stable, valid_frame,
        DisplayPayload, GenerationTracker, MainDisplayMode, DEFAULT_MAIN_DISPLAY_MODE,
        SHM_FORMAT_RGBA8888, SHM_MAGIC, SHM_PIXELS_OFFSET,
    };
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    static NEXT_TEST_FILE: AtomicU64 = AtomicU64::new(0);

    struct TestMap {
        mmap: Arc<memmap2::Mmap>,
        path: PathBuf,
    }

    impl TestMap {
        fn new(generation: u32, width: u32, height: u32, stride: u32, pixels: &[u8]) -> Self {
            let unique = NEXT_TEST_FILE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "cdj3k-main-stream-test-{}-{}-{unique}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let size = SHM_PIXELS_OFFSET + pixels.len();
            let mut file = OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(&path)
                .unwrap();
            file.set_len(size as u64).unwrap();
            let mut header = [0u8; SHM_PIXELS_OFFSET];
            header[0..4].copy_from_slice(&SHM_MAGIC.to_le_bytes());
            header[4..8].copy_from_slice(&generation.to_le_bytes());
            header[8..12].copy_from_slice(&width.to_le_bytes());
            header[12..16].copy_from_slice(&height.to_le_bytes());
            header[16..20].copy_from_slice(&stride.to_le_bytes());
            header[20..24].copy_from_slice(&SHM_FORMAT_RGBA8888.to_le_bytes());
            header[24..28].copy_from_slice(&0u32.to_le_bytes());
            header[28..32].copy_from_slice(&0u32.to_le_bytes());
            header[32..36].copy_from_slice(&width.to_le_bytes());
            header[36..40].copy_from_slice(&height.to_le_bytes());
            file.write_all(&header).unwrap();
            file.write_all(pixels).unwrap();
            file.sync_all().unwrap();
            let mmap = Arc::new(unsafe { memmap2::Mmap::map(&file).unwrap() });
            Self { mmap, path }
        }
    }

    impl Drop for TestMap {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[test]
    fn legacy_is_the_default_display_contract() {
        assert_eq!(
            MainDisplayMode::default(),
            MainDisplayMode::LegacyPublishedGeneration
        );
        assert_eq!(
            DEFAULT_MAIN_DISPLAY_MODE,
            MainDisplayMode::LegacyPublishedGeneration
        );
    }

    #[test]
    fn rejects_invalid_frame_headers() {
        let sequenced = MainDisplayMode::SequencedStableSnapshot;
        assert!(!valid_frame(
            sequenced,
            0,
            720,
            5120,
            SHM_FORMAT_RGBA8888,
            8 << 20
        ));
        assert!(!valid_frame(sequenced, 1280, 720, 5120, 0, 8 << 20));
        assert!(!valid_frame(
            sequenced,
            1280,
            720,
            5120,
            SHM_FORMAT_RGBA8888,
            SHM_PIXELS_OFFSET
        ));
        assert!(!valid_frame(
            sequenced,
            usize::MAX,
            2,
            usize::MAX,
            SHM_FORMAT_RGBA8888,
            usize::MAX
        ));
        assert!(valid_frame(
            MainDisplayMode::LegacyPublishedGeneration,
            4,
            2,
            16,
            0,
            SHM_PIXELS_OFFSET + 32
        ));
    }

    #[test]
    fn sequenced_stability_requires_same_nonzero_even_generation() {
        assert!(!generation_is_stable(0, 0));
        assert!(!generation_is_stable(1, 1));
        assert!(!generation_is_stable(2, 4));
        assert!(generation_is_stable(2, 2));
    }

    #[test]
    fn legacy_accepts_every_changed_generation_and_preserves_initial_wait() {
        let mut tracker = GenerationTracker::new(MainDisplayMode::LegacyPublishedGeneration, 1);
        assert!(tracker.observe(1).is_none());
        for generation in [2, 3, 4] {
            let event = tracker.observe(generation).unwrap();
            assert!(event.count_immediately);
        }
    }

    #[test]
    fn sequenced_accepts_initial_even_frame_then_only_stable_publications() {
        let mut tracker = GenerationTracker::new(MainDisplayMode::SequencedStableSnapshot, 2);
        assert!(!tracker.observe(2).unwrap().count_immediately);
        assert!(tracker.observe(3).is_none());
        assert!(!tracker.observe(4).unwrap().count_immediately);
        assert!(tracker.observe(4).is_none());

        let mut zero = GenerationTracker::new(MainDisplayMode::SequencedStableSnapshot, 0);
        assert!(zero.observe(0).is_none());
        assert!(zero.observe(1).is_none());
        assert!(zero.observe(2).is_some());
    }

    #[test]
    fn dirty_rectangles_accumulate_to_their_union() {
        let acc = accumulate_dirty(None, 10, 20, 5, 7).unwrap();
        assert_eq!(acc, (10, 20, 15, 27));
        let acc = accumulate_dirty(Some(acc), 4, 24, 20, 10).unwrap();
        assert_eq!(acc, (4, 20, 24, 34));
        assert!(accumulate_dirty(None, usize::MAX, 0, 2, 1).is_none());
    }

    #[test]
    fn stable_copy_packs_rows_and_rejects_changed_or_odd_generation() {
        let map = TestMap::new(2, 4, 2, 16, &(1u8..=32).collect::<Vec<_>>());
        let pixels = copy_stable_rect(&map.mmap, 2, 1, 0, 2, 2, 16, 4, 2).unwrap();
        assert_eq!(
            pixels,
            vec![5, 6, 7, 8, 9, 10, 11, 12, 21, 22, 23, 24, 25, 26, 27, 28]
        );
        assert!(copy_stable_rect(&map.mmap, 1, 1, 0, 2, 2, 16, 4, 2).is_none());
        assert!(copy_stable_rect(&map.mmap, 4, 1, 0, 2, 2, 16, 4, 2).is_none());
    }

    #[test]
    fn legacy_payload_is_mapped_with_surface_stride() {
        let map = TestMap::new(3, 4, 2, 20, &(1u8..=40).collect::<Vec<_>>());
        let dirty = build_display_dirty(
            &map.mmap,
            MainDisplayMode::LegacyPublishedGeneration,
            3,
            1,
            0,
            2,
            2,
            20,
            4,
            2,
        )
        .unwrap();
        assert_eq!(
            (dirty.x, dirty.y, dirty.w, dirty.h, dirty.stride),
            (1, 0, 2, 2, 20)
        );
        match dirty.payload {
            DisplayPayload::Mapped(mapped) => assert!(Arc::ptr_eq(&mapped, &map.mmap)),
            DisplayPayload::Owned(_) => panic!("legacy mode unexpectedly copied pixels"),
        }
    }

    #[test]
    fn sequenced_payload_is_owned_and_tightly_packed() {
        let map = TestMap::new(2, 4, 2, 20, &(1u8..=40).collect::<Vec<_>>());
        let dirty = build_display_dirty(
            &map.mmap,
            MainDisplayMode::SequencedStableSnapshot,
            2,
            1,
            0,
            2,
            2,
            20,
            4,
            2,
        )
        .unwrap();
        assert_eq!(
            (dirty.x, dirty.y, dirty.w, dirty.h, dirty.stride),
            (1, 0, 2, 2, 8)
        );
        match dirty.payload {
            DisplayPayload::Owned(pixels) => assert_eq!(
                pixels,
                vec![5, 6, 7, 8, 9, 10, 11, 12, 25, 26, 27, 28, 29, 30, 31, 32]
            ),
            DisplayPayload::Mapped(_) => panic!("sequenced mode did not own its snapshot"),
        }
    }
}
