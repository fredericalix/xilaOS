//! The memory of the machine: free pages, and the heap of the kernel.
//!
//! The bookkeeping is in `xila_core::mem`. This file gives it the free
//! memory the firmware reported and makes it the allocator behind `alloc`
//! (`Box`, `Vec`, `String`). Tasks can allocate at the same time, so every
//! access happens with interrupts off; there is one processor.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;

use xila_core::mem::{Heap, Pages, PAGE};

use crate::arch;

struct Memory {
    pages: Pages,
    heap: Heap,
}

struct Shared(UnsafeCell<Memory>);
unsafe impl Sync for Shared {}

static MEMORY: Shared = Shared(UnsafeCell::new(Memory { pages: Pages::new(), heap: Heap::new() }));

/// Runs `f` on the memory state with interrupts off.
fn with<R>(f: impl FnOnce(&mut Memory) -> R) -> R {
    let on = arch::interrupts_save();
    let result = f(unsafe { &mut *MEMORY.0.get() });
    arch::interrupts_restore(on);
    result
}

/// Puts the free regions reported by the firmware under management.
pub fn init(free: &[(usize, usize)]) {
    with(|memory| {
        for &(start, pages) in free {
            memory.pages.add(start, pages);
        }
    });
}

/// Takes whole pages. Returns the address of the first one.
pub fn pages(count: usize) -> Option<usize> {
    with(|memory| memory.pages.alloc(count)).map(|page| page * PAGE)
}

/// Gives back pages taken with `pages`.
pub fn free_pages(address: usize, count: usize) {
    with(|memory| memory.pages.free(address / PAGE, count));
}

pub struct Stats {
    pub managed_kb: u64,
    pub free_kb: u64,
    pub heap_used: u64,
}

pub fn stats() -> Stats {
    with(|memory| Stats {
        managed_kb: (memory.pages.total_pages() * PAGE / 1024) as u64,
        free_kb: (memory.pages.free_pages() * PAGE / 1024) as u64,
        heap_used: memory.heap.used() as u64,
    })
}

struct KernelHeap;

unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        with(|memory| memory.heap.alloc(&mut memory.pages, layout.size(), layout.align()))
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        with(|memory| memory.heap.dealloc(&mut memory.pages, ptr, layout.size(), layout.align()))
    }
}

#[global_allocator]
static HEAP: KernelHeap = KernelHeap;

/// Uses the heap the way the rest of the kernel will, and checks that
/// everything comes back. Returns false if something is wrong.
pub fn self_test() -> bool {
    use alloc::boxed::Box;
    use alloc::string::String;
    use alloc::vec::Vec;

    let before = stats();
    let ok = {
        let numbers: Vec<u64> = (0..5000).collect();
        let mut text = String::new();
        for i in 0..200 {
            text.push((b'a' + (i % 26) as u8) as char);
        }
        let boxed = Box::new([7u8; 3000]);
        let lists: Vec<Vec<u8>> = (0..50).map(|n| alloc::vec![n as u8; 10 + n * 7]).collect();
        numbers.iter().sum::<u64>() == 4999 * 5000 / 2
            && text.len() == 200
            && boxed.iter().all(|&b| b == 7)
            && lists.iter().enumerate().all(|(n, list)| list.len() == 10 + n * 7 && list.iter().all(|&b| b == n as u8))
    };
    let after = stats();
    ok && after.heap_used == before.heap_used
}
