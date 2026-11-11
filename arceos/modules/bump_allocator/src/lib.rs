#![no_std]

use allocator::{AllocError, AllocResult, BaseAllocator, ByteAllocator, PageAllocator};
use core::alloc::Layout;
use core::ptr::NonNull;

#[inline]
fn align_up(pos: usize, align: usize) -> usize {
    (pos + align - 1) & !(align - 1)
}

#[inline]
fn align_down(pos: usize, align: usize) -> usize {
    pos & !(align - 1)
}

/// Early memory allocator
/// Use it before formal bytes-allocator and pages-allocator can work!
/// This is a double-end memory range:
/// - Alloc bytes forward
/// - Alloc pages backward
///
/// [ bytes-used | avail-area | pages-used ]
/// |            | -->    <-- |            |
/// start       b_pos        p_pos       end
///
/// For bytes area, 'count' records number of allocations.
/// When it goes down to ZERO, free bytes-used area.
/// For pages area, it will never be freed!
///
pub struct EarlyAllocator<const SIZE: usize> {
    base: usize,
    end: usize,
    /// Byte allocations grow forward from `base`.
    b_pos: usize,
    /// Page allocations grow backward from `end`.
    p_pos: usize,
    /// Total size of live byte allocations.
    b_used: usize,
    /// Number of live byte allocations.
    b_count: usize,
}

impl<const SIZE: usize> EarlyAllocator<SIZE> {
    pub const fn new() -> Self {
        Self {
            base: 0,
            end: 0,
            b_pos: 0,
            p_pos: 0,
            b_used: 0,
            b_count: 0,
        }
    }
}

impl<const SIZE: usize> BaseAllocator for EarlyAllocator<SIZE> {
    fn init(&mut self, start: usize, size: usize) {
        self.base = start;
        self.end = start + size;
        self.b_pos = start;
        self.p_pos = start + size;
        self.b_used = 0;
        self.b_count = 0;
    }

    fn add_memory(&mut self, start: usize, size: usize) -> AllocResult {
        if self.end == 0 {
            self.init(start, size);
            return Ok(());
        }
        if self.b_count == 0 && start + size == self.base {
            // Extend the memory range backward before any byte allocation.
            self.base = start;
            self.b_pos = start;
            return Ok(());
        }
        if start == self.end {
            // Extend the memory range forward (only page area is in use).
            self.end = start + size;
            return Ok(());
        }
        Err(AllocError::InvalidParam)
    }
}

impl<const SIZE: usize> ByteAllocator for EarlyAllocator<SIZE> {
    fn alloc(&mut self, layout: Layout) -> AllocResult<NonNull<u8>> {
        let aligned = align_up(self.b_pos, layout.align());
        let new_pos = aligned + layout.size();
        if new_pos > self.p_pos {
            return Err(AllocError::NoMemory);
        }
        self.b_pos = new_pos;
        self.b_used += layout.size();
        self.b_count += 1;
        NonNull::new(aligned as *mut u8).ok_or(AllocError::NoMemory)
    }

    fn dealloc(&mut self, _pos: NonNull<u8>, layout: Layout) {
        self.b_used -= layout.size();
        self.b_count = self.b_count.saturating_sub(1);
        if self.b_count == 0 {
            // All byte allocations are freed, reset the bump pointer.
            self.b_pos = self.base;
            self.b_used = 0;
        }
    }

    fn total_bytes(&self) -> usize {
        self.end - self.base
    }

    fn used_bytes(&self) -> usize {
        self.b_used
    }

    fn available_bytes(&self) -> usize {
        self.p_pos - self.b_pos
    }
}

impl<const SIZE: usize> PageAllocator for EarlyAllocator<SIZE> {
    const PAGE_SIZE: usize = SIZE;

    fn alloc_pages(
        &mut self,
        num_pages: usize,
        align_pow2: usize,
    ) -> AllocResult<usize> {
        let align = align_up(1 << align_pow2, Self::PAGE_SIZE);
        let aligned_end = align_down(self.p_pos, align);
        let new_pos = aligned_end - num_pages * Self::PAGE_SIZE;
        if aligned_end < new_pos || new_pos < self.b_pos {
            return Err(AllocError::NoMemory);
        }
        self.p_pos = new_pos;
        Ok(new_pos)
    }

    fn dealloc_pages(&mut self, _pos: usize, _num_pages: usize) {
        // Page allocations will never be freed.
    }

    fn total_pages(&self) -> usize {
        (self.end - self.base) / Self::PAGE_SIZE
    }

    fn used_pages(&self) -> usize {
        (self.end - self.p_pos) / Self::PAGE_SIZE
    }

    fn available_pages(&self) -> usize {
        (self.p_pos - self.b_pos) / Self::PAGE_SIZE
    }
}
