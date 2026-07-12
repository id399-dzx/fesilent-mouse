//! 事件管道：捕获线程 → 消费线程的无锁通道（DEVELOPMENT.md §6.2）。
//!
//! 热路径约束：`Producer::push` 在钩子/tap 回调内调用，必须 O(1)、零分配、
//! 无锁（TECH-REVIEW A-2/A-3 回调时限）。实现为预分配 `ArrayQueue` +
//! 原子标志唤醒（`thread::unpark` 对调用方无锁）。
//!
//! 背压策略：队列满时丢弃**新**事件并计数（容量应配置为仅在病态场景才会
//! 打满；鼠标移动丢帧无害——影子坐标是绝对量，TECH-REVIEW A-4）。
//! 按键事件的不可丢性由上层保证：捕获层对 Key/Button 使用 `push_spin`，
//! 在极短自旋窗口内重试，仍失败才丢弃（此时连接早已不可用）。

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::Thread;
use std::time::Duration;

use crossbeam_queue::ArrayQueue;

struct Inner<T> {
    q: ArrayQueue<T>,
    /// 队列满被丢弃的事件数（监控指标）
    dropped: AtomicU64,
    /// 消费者是否处于 park 等待
    waiting: AtomicBool,
    /// 生产者是否仍存活（drop 时置 false，消费者据此终止）
    producer_alive: AtomicBool,
    /// 消费线程句柄（首次 recv 时注册，用于 unpark 唤醒）
    consumer: OnceLock<Thread>,
}

/// 创建容量固定的事件管道（SPSC 用法：一个捕获线程、一个消费线程）。
pub fn event_pipe<T>(capacity: usize) -> (Producer<T>, Consumer<T>) {
    let inner = Arc::new(Inner {
        q: ArrayQueue::new(capacity),
        dropped: AtomicU64::new(0),
        waiting: AtomicBool::new(false),
        producer_alive: AtomicBool::new(true),
        consumer: OnceLock::new(),
    });
    (
        Producer {
            inner: Arc::clone(&inner),
        },
        Consumer { inner },
    )
}

pub struct Producer<T> {
    inner: Arc<Inner<T>>,
}

impl<T> Producer<T> {
    /// 热路径入队。队列满 → 丢弃并计数，返回 false。零分配、无锁。
    #[inline]
    pub fn push(&self, ev: T) -> bool {
        let ok = match self.inner.q.push(ev) {
            Ok(()) => true,
            Err(_) => {
                self.inner.dropped.fetch_add(1, Ordering::Relaxed);
                false
            }
        };
        self.wake();
        ok
    }

    /// 不可丢事件（按键/抬起）入队：队列满时在有限自旋窗口内重试。
    /// 仍满则丢弃计数（此时管道消费侧已停摆，连接必然同时异常）。
    #[inline]
    pub fn push_spin(&self, mut ev: T, max_spins: u32) -> bool {
        for _ in 0..max_spins {
            match self.inner.q.push(ev) {
                Ok(()) => {
                    self.wake();
                    return true;
                }
                Err(back) => {
                    ev = back;
                    std::hint::spin_loop();
                }
            }
        }
        self.inner.dropped.fetch_add(1, Ordering::Relaxed);
        self.wake();
        false
    }

    /// 累计丢弃计数。
    pub fn dropped(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }

    #[inline]
    fn wake(&self) {
        if self.inner.waiting.load(Ordering::Acquire)
            && let Some(t) = self.inner.consumer.get()
        {
            t.unpark();
        }
    }
}

impl<T> Drop for Producer<T> {
    fn drop(&mut self) {
        self.inner.producer_alive.store(false, Ordering::Release);
        if let Some(t) = self.inner.consumer.get() {
            t.unpark();
        }
    }
}

pub struct Consumer<T> {
    inner: Arc<Inner<T>>,
}

impl<T> Consumer<T> {
    /// 非阻塞取一个事件。
    #[inline]
    pub fn try_recv(&self) -> Option<T> {
        self.inner.q.pop()
    }

    /// 阻塞取事件：短自旋 → park 等待唤醒。
    /// 返回 None 表示生产者已销毁且队列已排空（管道关闭）。
    pub fn recv(&self) -> Option<T> {
        // 注册本线程供生产者唤醒（幂等）
        let _ = self.inner.consumer.set(std::thread::current());
        loop {
            // 1) 快路径 + 短自旋
            for _ in 0..64 {
                if let Some(v) = self.inner.q.pop() {
                    return Some(v);
                }
                std::hint::spin_loop();
            }
            if !self.inner.producer_alive.load(Ordering::Acquire) {
                // 生产者已没：最后一次排空检查
                return self.inner.q.pop();
            }
            // 2) park 等待；park_timeout 兜底防唤醒丢失竞态
            self.inner.waiting.store(true, Ordering::Release);
            if let Some(v) = self.inner.q.pop() {
                self.inner.waiting.store(false, Ordering::Release);
                return Some(v);
            }
            std::thread::park_timeout(Duration::from_millis(1));
            self.inner.waiting.store(false, Ordering::Release);
        }
    }

    /// 累计丢弃计数。
    pub fn dropped(&self) -> u64 {
        self.inner.dropped.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn 单线程入出保序() {
        let (tx, rx) = event_pipe::<u32>(8);
        for i in 0..8 {
            assert!(tx.push(i));
        }
        for i in 0..8 {
            assert_eq!(rx.try_recv(), Some(i));
        }
        assert_eq!(rx.try_recv(), None);
    }

    #[test]
    fn 队列满丢新并计数() {
        let (tx, rx) = event_pipe::<u32>(2);
        assert!(tx.push(1));
        assert!(tx.push(2));
        assert!(!tx.push(3)); // 满，丢弃
        assert_eq!(tx.dropped(), 1);
        assert_eq!(rx.try_recv(), Some(1)); // 旧事件保留
    }

    #[test]
    fn 并发百万事件_零数据丢失_保序() {
        let (tx, rx) = event_pipe::<u64>(4096);
        const N: u64 = 1_000_000;
        let producer = std::thread::spawn(move || {
            for i in 0..N {
                // 外层 while 重投保证“数据级零丢失”：push_spin 自旋预算耗尽（队列满）时
                // 返回 false 并使 dropped++，但同一个 i 会被立刻重投，数据不会真正丢失。
                while !tx.push_spin(i, 1024) {}
            }
            tx.dropped()
        });
        let consumer = std::thread::spawn(move || {
            let mut expect = 0u64;
            while let Some(v) = rx.recv() {
                assert_eq!(v, expect, "事件乱序或丢失");
                expect += 1;
            }
            expect
        });
        let dropped = producer.join().unwrap();
        let received = consumer.join().unwrap();

        // 真正的无丢失/保序不变量（严格断言）：每个事件恰好被有序消费一次。
        // 若队列存在内存序/竞态 bug 丢事件或乱序，这两条之一必然失败。
        assert_eq!(received, N, "数据级丢失：收到事件数不等于发送数");

        // 注意 dropped 不是正确性指标，不能断言为 0：本工作负载（1M 事件灌 4096 环形
        // 队列、消费者更慢）会持续打满队列；push_spin 的自旋预算在消费线程被 OS 调度
        // 延迟时耗尽即计数（见文件头“背压策略” + TECH-REVIEW A-4，满即丢是既定设计）。
        // 这里外层 while 重投把每次“丢弃”都补偿成功投递，故 dropped 可 >0 而数据不丢。
        // 断言 dropped==0 是对调度时序的错误假设，会导致 flaky，故移除。
        let _ = dropped;
    }

    #[test]
    fn 生产者销毁后消费者终止() {
        let (tx, rx) = event_pipe::<u8>(4);
        tx.push(9);
        drop(tx);
        assert_eq!(rx.recv(), Some(9)); // 先排空
        assert_eq!(rx.recv(), None); // 再终止
    }
}
