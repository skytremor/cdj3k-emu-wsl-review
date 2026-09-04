//! MainLcdStream - reads the main LCD from the QEMU shm display backend.
//!
//! Shm file layout (written by qemu/patch/shm-display.c):
//!
//!   [0]   u32  magic       0x514D5348  ("QMS\x00")
//!   [4]   u32  generation  incremented with RELEASE after every dirty blit
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
//! The reader polls `generation` with Acquire semantics; when it changes,
//! dirty_x/y/w/h and pixel data are copied into an owned buffer and the
//! generation is checked again before publication. This prevents QEMU from
//! modifying the live mmap while the UI/OpenGL thread consumes a frame.
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

/// A dirty-region notification with an owned, stable pixel snapshot.
pub struct DisplayDirty {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    /// Row stride in bytes. For owned snapshots this is `w * 4`.
    pub stride: u32,
    /// Packed RGBA rows for this dirty rectangle.
    pub pixels: Vec<u8>,
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
        Self::new_with_control_gate(socket_dir, gate, None)
    }

    pub fn new_with_control_gate(
        socket_dir: &str,
        gate: crate::RepaintGate,
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
    control_gate: Option<Arc<crate::ControlConnectionGate>>,
) {
    let mut wait_logged = false;
    loop {
        // Wait for the shm file to appear and contain a valid header.
        let mmap = loop {
            match open_shm(shm_path) {
                Some(m) => {
                    eprintln!("[main_stream] opened {shm_path}");
                    wait_logged = false;
                    connected.store(true, Ordering::Relaxed);
                    gate.request();
                    break m;
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

        poll_loop(&mmap, &slot, &frames_seen, &gate, control_gate.as_ref());

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
    control_gate: Option<&Arc<crate::ControlConnectionGate>>,
) {
    // Treat the already-published generation as the first event. QEMU writes
    // an initial full frame before the host reader can attach; subtracting one
    // makes that static frame visible instead of waiting for the next damage.
    let mut last_gen: u32 = read_u32(mmap, 4).wrapping_sub(1);

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

        // Acquire load of generation - pairs with QEMU's RELEASE add.
        let gen = read_u32_acquire(mmap, 4);

        if gen == last_gen {
            continue;
        }
        puffin::profile_scope!("main_lcd_gen_bump");
        last_gen = gen;
        // Re-read dimensions on every frame - the surface can switch
        // mid-session (e.g. initial 640×480 QEMU console → 1280×720 Xorg).
        let width = read_u32(mmap, 8) as usize;
        let height = read_u32(mmap, 12) as usize;
        let stride = read_u32(mmap, 16) as usize;
        let format = read_u32(mmap, 20);

        if !valid_frame(width, height, stride, format, mmap.len()) {
            continue;
        }

        if let Some(control_gate) = control_gate {
            control_gate.observe_main(gen, width as u32, height as u32);
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
        if !dirty_valid && !first_frame && !surface_changed {
            continue;
        }

        let (dx, dy, dw, dh) = if first_frame || surface_changed {
            (0, 0, width, height)
        } else {
            (dx, dy, dw, dh)
        };

        // Expand the local accumulator to cover this dirty rect.
        acc = Some(match acc {
            None => (dx, dy, dx + dw, dy + dh),
            Some((ax0, ay0, ax1, ay1)) => {
                (ax0.min(dx), ay0.min(dy), ax1.max(dx + dw), ay1.max(dy + dh))
            }
        });

        // Try to publish the accumulated region.  If the slot is still
        // occupied the accumulator keeps growing - next tick will cover
        // everything that was missed.
        if let Ok(mut g) = slot.try_lock() {
            if g.is_none() {
                if let Some((x0, y0, x1, y1)) = acc.take() {
                    let uw = x1 - x0;
                    let uh = y1 - y0;

                    if let Some(pixels) =
                        copy_stable_rect(mmap, gen, x0, y0, uw, uh, stride, width, height)
                    {
                        *g = Some(DisplayDirty {
                            x: x0 as u32,
                            y: y0 as u32,
                            w: uw as u32,
                            h: uh as u32,
                            stride: (uw * 4) as u32,
                            pixels,
                        });
                        frames_seen.fetch_add(1, Ordering::Relaxed);
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

fn valid_frame(width: usize, height: usize, stride: usize, format: u32, map_len: usize) -> bool {
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
        && format == SHM_FORMAT_RGBA8888
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
    if read_u32_acquire(mmap, 4) != generation {
        return None;
    }
    Some(pixels)
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
    use super::{copy_stable_rect, valid_frame, SHM_FORMAT_RGBA8888, SHM_MAGIC, SHM_PIXELS_OFFSET};
    use std::fs::OpenOptions;
    use std::io::Write;

    #[test]
    fn rejects_invalid_frame_headers() {
        assert!(!valid_frame(0, 720, 5120, SHM_FORMAT_RGBA8888, 8 << 20));
        assert!(!valid_frame(1280, 720, 5120, 0, 8 << 20));
        assert!(!valid_frame(
            1280,
            720,
            5120,
            SHM_FORMAT_RGBA8888,
            SHM_PIXELS_OFFSET
        ));
        assert!(!valid_frame(
            usize::MAX,
            2,
            usize::MAX,
            SHM_FORMAT_RGBA8888,
            usize::MAX
        ));
    }

    #[test]
    fn copies_owned_rect_only_after_stable_generation() {
        let path = std::env::temp_dir().join(format!(
            "cdj3k-main-stream-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let size = SHM_PIXELS_OFFSET + 32 + 16;
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.set_len(size as u64).unwrap();
        let mut header = [0u8; SHM_PIXELS_OFFSET];
        header[0..4].copy_from_slice(&SHM_MAGIC.to_le_bytes());
        header[4..8].copy_from_slice(&1u32.to_le_bytes());
        header[8..12].copy_from_slice(&4u32.to_le_bytes());
        header[12..16].copy_from_slice(&2u32.to_le_bytes());
        header[16..20].copy_from_slice(&16u32.to_le_bytes());
        header[20..24].copy_from_slice(&SHM_FORMAT_RGBA8888.to_le_bytes());
        file.write_all(&header).unwrap();
        file.write_all(&(0u32).to_le_bytes()).unwrap();
        file.write_all(&(0u32).to_le_bytes()).unwrap();
        file.write_all(&(4u32).to_le_bytes()).unwrap();
        file.write_all(&(2u32).to_le_bytes()).unwrap();
        file.write_all(&[0x11; 32]).unwrap();
        file.sync_all().unwrap();
        let mmap = unsafe { memmap2::Mmap::map(&file).unwrap() };
        let pixels = copy_stable_rect(&mmap, 1, 0, 0, 4, 2, 16, 4, 2).unwrap();
        assert_eq!(pixels.len(), 32);
        std::fs::remove_file(path).unwrap();
    }
}
