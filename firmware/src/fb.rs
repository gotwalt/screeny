//! The decoded-frame handoff between the network half and the display half.
//!
//! Card 007 measured the problem this solves: the display task holds a frame
//! lock across a ~3 ms synchronous render, 154 times a second, so on a single
//! executor the frame task never gets to run while a render is in flight and
//! "drain the socket, newest wins" could not fire even once. Card 008 moves
//! the display onto core 1, and then a *lock* between the two halves would be
//! worse than useless: the consumer holds its frame for the whole 6.5 ms
//! refresh period (temporal dithering rewrites the DMA buffer every refresh
//! from the same sRGB frame), so a producer that waited for it would wait
//! most of the time, on the other core.
//!
//! So this is the standard lock-free triple buffer. Three slots; the producer
//! owns exactly one, the consumer owns exactly one, and one atomic word names
//! the third - the most recently published frame - plus a flag saying whether
//! the consumer has seen it yet. Publishing and acquiring are each one atomic
//! swap. Nothing blocks, nothing is copied, and no interrupt is ever disabled,
//! which matters because the HUB75 refresh ISR runs at `Priority3`.
//!
//! The invariant that makes the `unsafe` sound: **the index a `Producer` holds
//! and the index a `Consumer` holds are never equal, and neither is ever equal
//! to the other's after a swap.** `publish` gives the producer whatever index
//! was in the shared word, which by construction is neither the producer's own
//! nor the consumer's; `acquire` gives the consumer the same guarantee. Each
//! slot therefore has exactly one owner at any instant, so `back_mut` and
//! `front` can never alias.
//!
//! [`Slots::split`] hands out one `Producer` and one `Consumer` and cannot be
//! called twice on the same [`Slots`], which is what stops a second writer
//! from appearing.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::display::Frame;

/// Bit in the shared word: the published slot has not been taken yet.
const FRESH: u32 = 0x4;
/// Slot index bits of the shared word.
const IDX: u32 = 0x3;

/// Three frame slots plus the one word that arbitrates them.
pub struct Slots {
    slots: [UnsafeCell<Frame>; 3],
    shared: AtomicU32,
    split: AtomicBool,
}

// SAFETY: the module invariant above. Every `&mut Frame` handed out comes
// from `Producer`, every `&Frame` from `Consumer`, and the swap protocol
// keeps their indices distinct, so the two never alias the same slot.
unsafe impl Sync for Slots {}

impl Slots {
    /// Three black frames, with slot 2 published-but-stale.
    pub const fn new() -> Self {
        Slots {
            slots: [
                UnsafeCell::new(Frame::new()),
                UnsafeCell::new(Frame::new()),
                UnsafeCell::new(Frame::new()),
            ],
            // Producer starts on 0, consumer on 1, so the spare is 2.
            shared: AtomicU32::new(2),
            split: AtomicBool::new(false),
        }
    }

    /// Hand out the one producer and the one consumer.
    ///
    /// # Panics
    ///
    /// If called more than once on the same `Slots`. Two producers would
    /// break the aliasing invariant, and this is the cheapest place to say so.
    pub fn split(&'static self) -> (Producer, Consumer) {
        assert!(
            !self.split.swap(true, Ordering::Relaxed),
            "fb::Slots::split called twice"
        );
        (Producer { s: self, w: 0 }, Consumer { s: self, r: 1 })
    }
}

/// The writing end. One per [`Slots`], lives on core 0's frame task.
pub struct Producer {
    s: &'static Slots,
    w: u32,
}

impl Producer {
    /// The slot to compose the next frame into.
    ///
    /// Its contents are whatever was left there two publishes ago, so a
    /// caller that does not overwrite every pixel must clear it first. The
    /// five v1 decoders all write all 6144 bytes on success.
    pub fn back(&mut self) -> &mut Frame {
        // SAFETY: `self.w` is this producer's exclusively owned slot.
        unsafe { &mut *self.s.slots[self.w as usize].get() }
    }

    /// Make the back buffer the newest frame and take a fresh one.
    ///
    /// The `AcqRel` swap is the release edge for every pixel written above:
    /// the consumer's `Acquire` in [`Consumer::acquire`] pairs with it, which
    /// is what stops core 1 from scanning out half a frame.
    pub fn publish(&mut self) {
        let old = self.s.shared.swap(self.w | FRESH, Ordering::AcqRel);
        self.w = old & IDX;
    }
}

/// The reading end. One per [`Slots`], lives on core 1's display task.
pub struct Consumer {
    s: &'static Slots,
    r: u32,
}

impl Consumer {
    /// Take the newest published frame if there is one that is new to us.
    ///
    /// Returns `true` if [`Consumer::front`] now points at a different frame.
    /// A consumer that never calls this keeps rendering the frame it has,
    /// which is exactly what the dither loop wants between arrivals.
    pub fn acquire(&mut self) -> bool {
        if self.s.shared.load(Ordering::Relaxed) & FRESH == 0 {
            return false;
        }
        let old = self.s.shared.swap(self.r, Ordering::AcqRel);
        self.r = old & IDX;
        true
    }

    /// The frame currently on the panel.
    pub fn front(&self) -> &Frame {
        // SAFETY: `self.r` is this consumer's exclusively owned slot.
        unsafe { &*self.s.slots[self.r as usize].get() }
    }
}
