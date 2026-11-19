//! Memory bookkeeping: which pages are free, and a heap cut out of pages.
//!
//! Nothing here touches memory it was not given, so both parts are tested on
//! the development machine with an ordinary buffer standing in for the RAM.
//! Addresses and page numbers are plain integers: the kernel uses memory at
//! its physical addresses.

/// Size of a page of memory.
pub const PAGE: usize = 4096;

const MAX_REGIONS: usize = 64;

/// A run of pages, by page number.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Region {
    pub start: usize,
    pub pages: usize,
}

/// The free pages: a list of regions, sorted, that never touch each other.
pub struct Pages {
    regions: [Region; MAX_REGIONS],
    count: usize,
    /// Pages handed to `add` since the start.
    total: usize,
}

impl Pages {
    pub const fn new() -> Pages {
        Pages { regions: [Region { start: 0, pages: 0 }; MAX_REGIONS], count: 0, total: 0 }
    }

    /// Puts memory under management. Returns false if the list is full.
    pub fn add(&mut self, start: usize, pages: usize) -> bool {
        let added = self.free(start, pages);
        if added {
            self.total += pages;
        }
        added
    }

    /// Gives pages back. They are merged with the free regions around them.
    /// Returns false if the list is full; the pages are then lost.
    pub fn free(&mut self, start: usize, pages: usize) -> bool {
        if pages == 0 {
            return true;
        }
        let at = self.regions[..self.count].iter().position(|r| r.start > start).unwrap_or(self.count);
        let joins_before = at > 0 && self.regions[at - 1].start + self.regions[at - 1].pages == start;
        let joins_after = at < self.count && start + pages == self.regions[at].start;
        match (joins_before, joins_after) {
            (true, true) => {
                self.regions[at - 1].pages += pages + self.regions[at].pages;
                self.regions.copy_within(at + 1..self.count, at);
                self.count -= 1;
            }
            (true, false) => self.regions[at - 1].pages += pages,
            (false, true) => {
                self.regions[at].start = start;
                self.regions[at].pages += pages;
            }
            (false, false) => {
                if self.count == MAX_REGIONS {
                    return false;
                }
                self.regions.copy_within(at..self.count, at + 1);
                self.regions[at] = Region { start, pages };
                self.count += 1;
            }
        }
        true
    }

    /// Takes `pages` pages in a row from the first region that has them.
    /// Returns the number of the first page.
    pub fn alloc(&mut self, pages: usize) -> Option<usize> {
        if pages == 0 {
            return None;
        }
        let at = self.regions[..self.count].iter().position(|r| r.pages >= pages)?;
        let start = self.regions[at].start;
        if self.regions[at].pages == pages {
            self.regions.copy_within(at + 1..self.count, at);
            self.count -= 1;
        } else {
            self.regions[at].start += pages;
            self.regions[at].pages -= pages;
        }
        Some(start)
    }

    pub fn free_pages(&self) -> usize {
        self.regions[..self.count].iter().map(|r| r.pages).sum()
    }

    pub fn total_pages(&self) -> usize {
        self.total
    }

    pub fn regions(&self) -> &[Region] {
        &self.regions[..self.count]
    }
}

/// Block sizes of the heap: 16 bytes, doubling up to 2048.
const CLASSES: usize = 8;
const SMALLEST: usize = 16;
const LARGEST: usize = SMALLEST << (CLASSES - 1);

/// The heap: memory for values whose size is only known while running.
///
/// Requests up to 2048 bytes get a block of the next power of two; a page is
/// cut into blocks of one size when a list runs out, and freed blocks go back
/// to the list of their size. Larger requests take whole pages and return
/// them when freed.
pub struct Heap {
    /// First free block of each size, 0 for none. A free block holds the
    /// address of the next one.
    free: [usize; CLASSES],
    /// Bytes handed out and not yet freed, counted in whole blocks and pages.
    used: usize,
}

/// The list a request belongs to, or `None` if it needs whole pages.
fn class(size: usize, align: usize) -> Option<usize> {
    let block = size.max(align).max(SMALLEST).next_power_of_two();
    if block > LARGEST {
        None
    } else {
        Some((block / SMALLEST).trailing_zeros() as usize)
    }
}

fn pages_for(size: usize) -> usize {
    size.div_ceil(PAGE)
}

impl Heap {
    pub const fn new() -> Heap {
        Heap { free: [0; CLASSES], used: 0 }
    }

    pub fn used(&self) -> usize {
        self.used
    }

    /// Returns memory for `size` bytes aligned to `align`, or null.
    ///
    /// # Safety
    /// The pages of `pages` must be memory this heap may write to.
    pub unsafe fn alloc(&mut self, pages: &mut Pages, size: usize, align: usize) -> *mut u8 {
        if align > PAGE {
            return core::ptr::null_mut();
        }
        let Some(class) = class(size, align) else {
            let count = pages_for(size);
            return match pages.alloc(count) {
                Some(page) => {
                    self.used += count * PAGE;
                    (page * PAGE) as *mut u8
                }
                None => core::ptr::null_mut(),
            };
        };
        let block = SMALLEST << class;
        if self.free[class] == 0 {
            let Some(page) = pages.alloc(1) else {
                return core::ptr::null_mut();
            };
            // Cut the page into blocks and chain them.
            let base = page * PAGE;
            for offset in (0..PAGE).step_by(block) {
                let next = if offset + block < PAGE { base + offset + block } else { 0 };
                ((base + offset) as *mut usize).write(next);
            }
            self.free[class] = base;
        }
        let taken = self.free[class];
        self.free[class] = (taken as *const usize).read();
        self.used += block;
        taken as *mut u8
    }

    /// Takes back memory returned by `alloc` with the same size and alignment.
    ///
    /// # Safety
    /// `ptr` must come from `alloc` of this heap and not be used afterwards.
    pub unsafe fn dealloc(&mut self, pages: &mut Pages, ptr: *mut u8, size: usize, align: usize) {
        match class(size, align) {
            Some(class) => {
                (ptr as *mut usize).write(self.free[class]);
                self.free[class] = ptr as usize;
                self.used -= SMALLEST << class;
            }
            None => {
                let count = pages_for(size);
                pages.free(ptr as usize / PAGE, count);
                self.used -= count * PAGE;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_are_taken_first_fit_and_merged_back() {
        let mut p = Pages::new();
        assert!(p.add(100, 10));
        assert!(p.add(200, 50));
        assert_eq!((p.free_pages(), p.total_pages()), (60, 60));
        assert_eq!(p.alloc(4), Some(100));
        assert_eq!(p.alloc(8), Some(200)); // the first region has only 6 left
        assert_eq!(p.alloc(6), Some(104)); // exactly what is left of it
        assert_eq!(p.regions(), [Region { start: 208, pages: 42 }]);
        assert_eq!(p.alloc(43), None);

        p.free(104, 6);
        p.free(100, 4); // joins the region after it
        assert_eq!(p.regions()[0], Region { start: 100, pages: 10 });
        p.free(200, 8); // joins the region after it
        assert_eq!(p.regions(), [Region { start: 100, pages: 10 }, Region { start: 200, pages: 50 }]);
        assert_eq!(p.free_pages(), 60);
    }

    #[test]
    fn freeing_between_two_regions_joins_them() {
        let mut p = Pages::new();
        p.add(10, 30);
        let a = p.alloc(10).unwrap();
        let b = p.alloc(10).unwrap();
        p.free(a, 10);
        assert_eq!(p.regions().len(), 2);
        p.free(b, 10);
        assert_eq!(p.regions(), [Region { start: 10, pages: 30 }]);
    }

    #[test]
    fn a_full_list_refuses_more_regions() {
        let mut p = Pages::new();
        for i in 0..MAX_REGIONS {
            assert!(p.add(i * 10, 1));
        }
        assert!(!p.add(5000, 1));
        assert!(p.free(1, 1)); // touches a region: no new entry needed
        assert_eq!(p.total_pages(), MAX_REGIONS);
    }

    /// Real memory for the heap to work in: 64 pages, page aligned.
    fn arena() -> (Vec<u8>, Pages) {
        let buffer = vec![0u8; 65 * PAGE];
        let first = (buffer.as_ptr() as usize).div_ceil(PAGE);
        let mut pages = Pages::new();
        pages.add(first, 64);
        (buffer, pages)
    }

    #[test]
    fn small_blocks_are_reused_by_size() {
        let (_buffer, mut pages) = arena();
        let mut heap = Heap::new();
        unsafe {
            let a = heap.alloc(&mut pages, 24, 8);
            let b = heap.alloc(&mut pages, 32, 8);
            assert!(!a.is_null() && !b.is_null() && a != b);
            assert_eq!(heap.used(), 64);
            assert_eq!(pages.free_pages(), 63); // one page was cut into blocks of 32
            a.write_bytes(0xaa, 24);
            b.write_bytes(0xbb, 32);
            assert_eq!(*a, 0xaa);
            heap.dealloc(&mut pages, a, 24, 8);
            let c = heap.alloc(&mut pages, 17, 1);
            assert_eq!(c, a); // the freed block is the next one of that size
            assert_eq!(*b, 0xbb);
            heap.dealloc(&mut pages, b, 32, 8);
            heap.dealloc(&mut pages, c, 17, 1);
            assert_eq!(heap.used(), 0);
        }
    }

    #[test]
    fn alignment_is_respected() {
        let (_buffer, mut pages) = arena();
        let mut heap = Heap::new();
        unsafe {
            for align in [1usize, 8, 64, 512, 4096] {
                let p = heap.alloc(&mut pages, 10, align);
                assert_eq!(p as usize % align, 0, "align {align}");
            }
            assert!(heap.alloc(&mut pages, 10, 8192).is_null());
        }
    }

    #[test]
    fn large_requests_take_pages_and_give_them_back() {
        let (_buffer, mut pages) = arena();
        let mut heap = Heap::new();
        unsafe {
            let big = heap.alloc(&mut pages, 3 * PAGE + 1, 8);
            assert!(!big.is_null());
            assert_eq!(pages.free_pages(), 60);
            assert_eq!(heap.used(), 4 * PAGE);
            heap.dealloc(&mut pages, big, 3 * PAGE + 1, 8);
            assert_eq!((pages.free_pages(), heap.used()), (64, 0));
            assert!(heap.alloc(&mut pages, 65 * PAGE, 8).is_null());
        }
    }

    #[test]
    fn many_blocks_do_not_overlap() {
        let (_buffer, mut pages) = arena();
        let mut heap = Heap::new();
        let mut blocks = Vec::new();
        unsafe {
            for i in 0..200usize {
                let size = 16 + (i * 37) % 900;
                let p = heap.alloc(&mut pages, size, 8);
                assert!(!p.is_null());
                p.write_bytes(i as u8, size);
                blocks.push((p, size, i as u8));
            }
            for &(p, size, mark) in &blocks {
                assert!((0..size).all(|k| *p.add(k) == mark));
            }
            for (p, size, _) in blocks {
                heap.dealloc(&mut pages, p, size, 8);
            }
        }
        assert_eq!(heap.used(), 0);
    }
}
