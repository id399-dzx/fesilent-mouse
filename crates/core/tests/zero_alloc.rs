//! T-102 验证：事件管道热路径零分配（DEVELOPMENT.md §6.2）。
//! 用计数分配器统计 push/pop 期间的堆分配次数，必须为 0。

// 例外说明：实现 GlobalAlloc 必须 unsafe。这是测试夹具（不进发布产物），
// 每处 unsafe 均有 SAFETY 注释，不违反 SPEC §7"unsafe 仅平台 crate"的产品红线。
#![allow(unsafe_code)]
#![allow(clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

struct CountingAlloc;

static ALLOC_COUNT: AtomicU64 = AtomicU64::new(0);

// SAFETY: 完全委托给 System 分配器，仅附加原子计数，不改变任何分配语义。
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        // SAFETY: 入参 layout 由调用方保证合法，直接转发。
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: ptr/layout 来自本分配器先前的 alloc，直接转发。
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static A: CountingAlloc = CountingAlloc;

#[test]
fn 热路径push_pop零分配() {
    // InputEvent 等价的 Copy 载荷（管道对 T 泛型，零分配性质与 T 无关，
    // 只要 T 本身不含堆数据——InputEvent 满足 Copy）
    #[derive(Clone, Copy)]
    struct Ev(#[allow(dead_code)] [u64; 4]); // 仅作占位载荷，模拟 InputEvent 的体积

    let (tx, rx) = ms_core::pipeline::event_pipe::<Ev>(1024);

    // 预热：让一次性懒初始化（消费线程注册等）发生在测量窗口外
    tx.push(Ev([0; 4]));
    rx.try_recv().unwrap();

    let before = ALLOC_COUNT.load(Ordering::Relaxed);
    for i in 0..100_000u64 {
        assert!(tx.push(Ev([i; 4])));
        assert!(rx.try_recv().is_some());
    }
    let after = ALLOC_COUNT.load(Ordering::Relaxed);

    assert_eq!(
        after - before,
        0,
        "热路径出现了 {} 次堆分配",
        after - before
    );
}
