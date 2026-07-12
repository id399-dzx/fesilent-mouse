//! 文件传输中心（FR-503/504）：守护进程的收发任务登记进度/状态，设置界面读出显示
//! 进度条、提供取消、以及 >1GB 大文件发送前的确认。
//!
//! 用进程级单例（一个进程同时只有一个 daemon + 一个 UI），避免把共享状态层层穿过
//! daemon 的多层函数签名。线程安全：列表用 `Mutex`（更新频率为每块 1 MiB，非热路径），
//! 取消/确认用原子量（UI 与收发任务共享同一 `Arc`）。

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

/// 大文件阈值（FR-504：>1 GiB 发送前需用户确认）。
pub const LARGE_FILE_THRESHOLD: u64 = 1024 * 1024 * 1024;

/// 完成/失败/取消的条目在列表中保留的数量上限（超出则丢弃最旧的已结束项）。
const MAX_FINISHED_KEPT: usize = 8;

/// 传输历史保留条数（T-809，最近 N 次，新的在前）。
const MAX_HISTORY: usize = 10;

/// 速率估算（指数滑动平均，T-809）。更新频率为每块 1 MiB，非热路径。
struct RateState {
    started: Instant,
    last_at: Instant,
    last_bytes: u64,
    rate_bps: f64,
}

impl RateState {
    /// 速率采样最小间隔：更新过密时只记字节不更新速率，避免 dt≈0 的抖动。
    const MIN_DT: Duration = Duration::from_millis(200);

    fn new(now: Instant) -> Self {
        Self {
            started: now,
            last_at: now,
            last_bytes: 0,
            rate_bps: 0.0,
        }
    }

    /// 进度采样：按间隔计算瞬时速率并做 EMA 平滑（0.3 新 + 0.7 旧）。
    fn advance(&mut self, now: Instant, transferred: u64) {
        let dt = now.duration_since(self.last_at);
        if dt < Self::MIN_DT {
            return;
        }
        let delta = transferred.saturating_sub(self.last_bytes) as f64;
        let inst = delta / dt.as_secs_f64();
        self.rate_bps = if self.rate_bps == 0.0 {
            inst
        } else {
            0.3 * inst + 0.7 * self.rate_bps
        };
        self.last_at = now;
        self.last_bytes = transferred;
    }

    /// 剩余时间估算（秒）；速率未知或已传完时返回 None。
    fn eta_secs(&self, total: u64, transferred: u64) -> Option<u64> {
        if self.rate_bps <= 1.0 || transferred >= total {
            return None;
        }
        Some(((total - transferred) as f64 / self.rate_bps).round() as u64)
    }
}

/// 一次已结束传输的历史记录（T-809，供设置窗「最近传输」渲染）。
#[derive(Clone)]
pub struct HistoryItem {
    pub name: String,
    pub dir: Dir,
    /// 实际传输字节（完成 = 文件总大小；失败/取消 = 中断前已传）。
    pub bytes: u64,
    /// 结束方式（仅 Done/Failed/Cancelled）。
    pub outcome: Phase,
    /// 起传到结束的耗时（秒）。
    pub secs: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Send,
    Recv,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    /// 等待用户确认（仅大文件发送）。
    Confirm,
    /// 传输中。
    Active,
    Done,
    Failed,
    Cancelled,
}

// 确认决定（原子量取值）。
const DECISION_PENDING: u8 = 0;
const DECISION_APPROVE: u8 = 1;
const DECISION_REJECT: u8 = 2;

struct Entry {
    id: u64,
    name: String,
    total: u64,
    transferred: u64,
    dir: Dir,
    phase: Phase,
    cancel: Arc<AtomicBool>,
    decision: Arc<AtomicU8>,
    rate: RateState,
}

/// 一条传输的只读视图（供 UI 渲染）。
#[derive(Clone)]
pub struct View {
    pub id: u64,
    pub name: String,
    pub total: u64,
    pub transferred: u64,
    pub dir: Dir,
    pub phase: Phase,
    /// 平滑速率（字节/秒）；未知为 0（T-809）。
    pub rate_bps: f64,
    /// 剩余时间估算（秒）；未知为 None（T-809）。
    pub eta_secs: Option<u64>,
}

/// 收发任务持有的控制句柄。
pub struct Handle {
    id: u64,
    cancel: Arc<AtomicBool>,
    decision: Arc<AtomicU8>,
}

impl Handle {
    /// 更新已传字节数（同时推进速率估算，T-809）。
    pub fn update(&self, transferred: u64) {
        let now = Instant::now();
        center().with(|list| {
            if let Some(e) = list.iter_mut().find(|e| e.id == self.id) {
                e.transferred = transferred;
                e.rate.advance(now, transferred);
            }
        });
    }

    /// 置为某结束/活动相位。转入终态时落一条历史记录（T-809）；
    /// 大文件确认后转 Active 时重置速率计时（确认等待不计入耗时/速率）。
    pub fn set_phase(&self, p: Phase) {
        let now = Instant::now();
        let hist = center().with(|list| {
            let e = list.iter_mut().find(|e| e.id == self.id)?;
            if e.phase == p {
                return None;
            }
            let prev = e.phase;
            e.phase = p;
            match p {
                Phase::Active => {
                    if prev == Phase::Confirm {
                        e.rate = RateState::new(now);
                    }
                    None
                }
                Phase::Done | Phase::Failed | Phase::Cancelled => Some(HistoryItem {
                    name: e.name.clone(),
                    dir: e.dir,
                    bytes: if p == Phase::Done {
                        e.total
                    } else {
                        e.transferred
                    },
                    outcome: p,
                    secs: now.duration_since(e.rate.started).as_secs(),
                }),
                Phase::Confirm => None,
            }
        });
        if let Some(h) = hist {
            center().push_history(h);
        }
    }

    /// 用户是否已请求取消。
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    /// 供文件传输直接复用的取消标志（与 ms-net transfer 的 `Arc<AtomicBool>` 对接）。
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }

    /// 等待大文件确认结果：返回 true=已确认发送，false=拒绝/超时/取消。
    /// 轮询决定原子量（UI 在另一线程置位），最长等待 `timeout_secs` 秒。
    pub async fn await_confirm(&self, timeout_secs: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);
        loop {
            match self.decision.load(Ordering::Acquire) {
                DECISION_APPROVE => return true,
                DECISION_REJECT => return false,
                _ => {}
            }
            if self.is_cancelled() || std::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        }
    }
}

pub struct TransferCenter {
    list: Mutex<Vec<Entry>>,
    next_id: AtomicU64,
    /// 有大文件待确认时置位，提示 UI 弹出窗口。
    show_request: AtomicBool,
    /// 最近 N 次已结束传输（新的在前，T-809）。
    history: Mutex<Vec<HistoryItem>>,
}

static CENTER: LazyLock<TransferCenter> = LazyLock::new(|| TransferCenter {
    list: Mutex::new(Vec::new()),
    next_id: AtomicU64::new(1),
    show_request: AtomicBool::new(false),
    history: Mutex::new(Vec::new()),
});

/// 进程级传输中心单例。
pub fn center() -> &'static TransferCenter {
    &CENTER
}

impl TransferCenter {
    fn with<R>(&self, f: impl FnOnce(&mut Vec<Entry>) -> R) -> R {
        let mut g = self.list.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut g)
    }

    /// 登记一条新传输。`Send` 且 `total > 阈值` 时初始相位为 `Confirm` 并请求弹窗。
    pub fn begin(&self, name: String, total: u64, dir: Dir) -> Handle {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let needs_confirm = dir == Dir::Send && total > LARGE_FILE_THRESHOLD;
        let phase = if needs_confirm {
            Phase::Confirm
        } else {
            Phase::Active
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let decision = Arc::new(AtomicU8::new(DECISION_PENDING));
        self.with(|list| {
            prune_finished(list);
            list.push(Entry {
                id,
                name,
                total,
                transferred: 0,
                dir,
                phase,
                cancel: Arc::clone(&cancel),
                decision: Arc::clone(&decision),
                rate: RateState::new(Instant::now()),
            });
        });
        if needs_confirm {
            self.show_request.store(true, Ordering::Release);
        }
        Handle {
            id,
            cancel,
            decision,
        }
    }

    /// UI：取列表快照渲染。速率/剩余时间仅对传输中的条目有值（T-809）。
    pub fn snapshot(&self) -> Vec<View> {
        self.with(|list| {
            list.iter()
                .map(|e| {
                    let active = e.phase == Phase::Active;
                    View {
                        id: e.id,
                        name: e.name.clone(),
                        total: e.total,
                        transferred: e.transferred,
                        dir: e.dir,
                        phase: e.phase,
                        rate_bps: if active { e.rate.rate_bps } else { 0.0 },
                        eta_secs: if active {
                            e.rate.eta_secs(e.total, e.transferred)
                        } else {
                            None
                        },
                    }
                })
                .collect()
        })
    }

    /// UI：最近 N 次已结束传输（新的在前，T-809）。
    pub fn history(&self) -> Vec<HistoryItem> {
        self.history
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 终态记录入历史，保留最近 MAX_HISTORY 条（新的在前）。
    fn push_history(&self, item: HistoryItem) {
        let mut h = self.history.lock().unwrap_or_else(|e| e.into_inner());
        h.insert(0, item);
        h.truncate(MAX_HISTORY);
    }

    /// UI：请求取消某传输（同时拒绝尚未确认的）。
    pub fn cancel(&self, id: u64) {
        self.with(|list| {
            if let Some(e) = list.iter_mut().find(|e| e.id == id) {
                e.cancel.store(true, Ordering::Release);
                e.decision.store(DECISION_REJECT, Ordering::Release);
            }
        });
    }

    /// UI：对待确认的大文件作出决定（true=确认发送，false=拒绝）。
    pub fn decide(&self, id: u64, approve: bool) {
        self.with(|list| {
            if let Some(e) = list.iter_mut().find(|e| e.id == id) {
                e.decision.store(
                    if approve {
                        DECISION_APPROVE
                    } else {
                        DECISION_REJECT
                    },
                    Ordering::Release,
                );
            }
        });
    }

    /// UI：是否有"需弹窗"的请求（读取即清除）。
    pub fn take_show_request(&self) -> bool {
        self.show_request.swap(false, Ordering::AcqRel)
    }

    /// 是否有进行中（含待确认）的传输。
    pub fn has_active(&self) -> bool {
        self.with(|list| {
            list.iter()
                .any(|e| matches!(e.phase, Phase::Active | Phase::Confirm))
        })
    }
}

/// 已结束（Done/Failed/Cancelled）的条目超过保留上限时，丢弃最旧的若干。
fn prune_finished(list: &mut Vec<Entry>) {
    let finished = |p: Phase| matches!(p, Phase::Done | Phase::Failed | Phase::Cancelled);
    let count = list.iter().filter(|e| finished(e.phase)).count();
    if count <= MAX_FINISHED_KEPT {
        return;
    }
    let mut to_drop = count - MAX_FINISHED_KEPT;
    list.retain(|e| {
        if to_drop > 0 && finished(e.phase) {
            to_drop -= 1;
            false
        } else {
            true
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    // 单测合一：transfers 用进程级单例 CENTER，分多个 #[test] 并行会互相干扰
    // （尤其全局 show_request），故顺序写在一个测试里，按各自 id 断言、不依赖列表全集。
    #[test]
    fn 传输中心_确认进度取消() {
        let c = center();

        // 大文件发送 → Confirm + 请求弹窗；小文件 → 直接 Active；接收端不弹确认
        let big = c.begin("big.bin".into(), LARGE_FILE_THRESHOLD + 1, Dir::Send);
        assert!(c.take_show_request(), "大文件发送应请求弹窗");
        assert!(!c.take_show_request(), "读取后清除");
        let small = c.begin("a.txt".into(), 1024, Dir::Send);
        let recv_big = c.begin("r.bin".into(), LARGE_FILE_THRESHOLD + 1, Dir::Recv);
        assert!(!c.take_show_request(), "小文件与接收不应请求弹窗");

        let phase = |id| {
            c.snapshot()
                .iter()
                .find(|v| v.id == id)
                .map(|v| v.phase)
                .unwrap()
        };
        assert_eq!(phase(big.id), Phase::Confirm, "大文件发送需确认");
        assert_eq!(phase(small.id), Phase::Active, "小文件直接传");
        assert_eq!(phase(recv_big.id), Phase::Active, "接收端不弹确认");

        // 进度更新可读
        big.update(500);
        assert_eq!(
            c.snapshot()
                .iter()
                .find(|v| v.id == big.id)
                .unwrap()
                .transferred,
            500
        );

        // 确认 → decision=APPROVE
        c.decide(big.id, true);
        assert_eq!(big.decision.load(Ordering::Acquire), DECISION_APPROVE);

        // 取消 → cancel 置位
        c.cancel(small.id);
        assert!(small.is_cancelled());
    }

    #[test]
    fn 速率与eta估算() {
        let t0 = Instant::now();
        let mut r = RateState::new(t0);
        // 1s 传 1 MiB → 瞬时即速率
        r.advance(t0 + Duration::from_secs(1), 1_048_576);
        assert!((r.rate_bps - 1_048_576.0).abs() < 1.0);
        // 再 1s 传到 3 MiB → 瞬时 2 MiB/s，EMA = 0.3×2 + 0.7×1 = 1.3 MiB/s
        r.advance(t0 + Duration::from_secs(2), 3 * 1_048_576);
        let expect = 0.3 * 2.0 * 1_048_576.0 + 0.7 * 1_048_576.0;
        assert!((r.rate_bps - expect).abs() < 1.0);
        // 间隔小于采样窗口（200ms）：不更新速率，防 dt≈0 抖动
        let before = r.rate_bps;
        r.advance(t0 + Duration::from_millis(2050), 4 * 1_048_576);
        assert!((r.rate_bps - before).abs() < f64::EPSILON);
        // ETA：总 16 MiB 已传 3 MiB，剩 13 MiB ÷ 1.3 MiB/s = 10s
        let total = 16 * 1_048_576u64;
        assert_eq!(r.eta_secs(total, 3 * 1_048_576), Some(10));
        // 已传完 / 速率未知 → None
        assert!(r.eta_secs(total, total).is_none());
        assert!(RateState::new(t0).eta_secs(total, 0).is_none());
    }

    #[test]
    fn 历史记录_终态落账新在前且封顶() {
        let c = center();
        let a = c.begin("hist-a.bin".into(), 2048, Dir::Recv);
        a.update(2048);
        a.set_phase(Phase::Done);
        let b = c.begin("hist-b.bin".into(), 4096, Dir::Send);
        b.update(1024);
        b.set_phase(Phase::Cancelled);
        b.set_phase(Phase::Cancelled); // 重复置同一终态不重复落账

        let h = c.history();
        let ia = h.iter().position(|x| x.name == "hist-a.bin").unwrap();
        let ib = h.iter().position(|x| x.name == "hist-b.bin").unwrap();
        assert!(ib < ia, "新的在前");
        assert_eq!(h[ia].outcome, Phase::Done);
        assert_eq!(h[ia].bytes, 2048, "完成按文件总大小");
        assert_eq!(h[ib].outcome, Phase::Cancelled);
        assert_eq!(h[ib].bytes, 1024, "取消按已传字节");
        assert_eq!(
            h.iter().filter(|x| x.name == "hist-b.bin").count(),
            1,
            "终态只落一次"
        );

        // 封顶：塞满后不超过 MAX_HISTORY
        for i in 0..MAX_HISTORY + 2 {
            let t = c.begin(format!("hist-cap-{i}.bin"), 1, Dir::Recv);
            t.set_phase(Phase::Failed);
        }
        assert!(c.history().len() <= MAX_HISTORY);
    }
}
