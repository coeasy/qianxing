//! 崩溃可恢复的文件写锁（V11 §40 D1）。
//!
//! 本框架原先有四处各自就地写的 `OpenOptions::create_new` 锁：数据集注册表、多腿订单簿状态、
//! 作业队列 claim、存储追加信封。它们的共同形状是"抢到就干活，释放时删文件"，而**释放依赖
//! 进程活着走到那一步**。Ctrl-C、OOM、断电、容器被杀都会把锁文件留在盘上，此后每一次运行都
//! 只拿到 `文件存在。 (os error 80)`：回测链、多腿链、作业队列当场永久断链，而报错既不点名
//! 锁在哪、也不给任何出路（`FileJobQueue` 那一处甚至把它报成 `LeaseHeld`，声称有并发持有者，
//! 而那个持有者早已不存在）。
//!
//! 这里收敛为唯一一份实现，判据与副作用分开：
//! - [`decide_lock`] 是纯函数（年龄 + 阈值 + 本场已接管与否 → 接管/等待），回测与故障注入可确定性复现；
//! - [`FileLock::acquire_with`] 只在边界读一次系统时钟，且**有界**：等待次数用尽即失败，
//!   接管后重新 `create_new` 也消耗同一计数，因此不存在"锁永远抢不到而死循环"的形状。
//!
//! 阈值的依据是被保护 critical section 的实际尺度：这几处都只做"读一个小 JSON、改内存、
//! 写临时文件、原子改名"，毫秒级。默认 30 秒比最慢的真实持锁高三个数量级，落到秒级反而会让
//! 正常并发写者把接管当成机会 —— 与 [`crate::retry`] 的判据同源，宁可等，不可抢错。
//!
//! 锁文件里写的是一份**所有权令牌**（`pid-计数-纳秒`），它有两个用途：失败信息能点名
//! "是谁的锁"，以及 [`FileLock::drop`] 只删自己那一把（V11 K3）。令牌**不是存活判定**：
//! Windows 会复用 PID，判活会把"恰好同号的无关进程"读成持有者。它回答的是"路径上这份文件
//! 还是不是我创建的那一份"——一个所有权问题，不涉及创建者此刻是死是活。

use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 锁文件年龄达到该阈值即视为孤儿，允许接管。
pub const DEFAULT_LOCK_STALE_AFTER: Duration = Duration::from_secs(30);
/// 锁仍新鲜时最多等待多少次（默认 100 × 1ms ≈ 100ms，与存储信封历史形状一致）。
pub const DEFAULT_LOCK_WAIT_ATTEMPTS: u32 = 100;
/// 每次等待之间睡多久。
pub const DEFAULT_LOCK_WAIT_INTERVAL: Duration = Duration::from_millis(1);

/// 一次锁竞争的判据结果。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockDecision {
    /// 锁不存在或已足够老、且本场竞争还没接管过：删除后重新抢。
    Takeover { age: Duration },
    /// 锁还新鲜（或已经接管过一次）：再等一轮。年龄读不到（正在被删除）时也走这一支，
    /// 宁可等不可抢错。
    Wait { age: Option<Duration> },
}

/// 纯判据：年龄达到阈值且本场竞争还没接管过才允许删除，读不到年龄一律等待。
///
/// `takeover_used` 是这条判据的一部分而不是调用处的一句 `if`：一次抢锁只删一次别人的锁，
/// 删掉之后重新抢若又看见一份过期锁，只能按竞争失败收场。若这个约束长在循环里，纯判据
/// 就答不出"同一场竞争里第二次见到孤儿锁"，而那条正是"两个写者互相删对方刚建的锁"的唯一出路。
pub fn decide_lock(
    age: Option<Duration>,
    stale_after: Duration,
    takeover_used: bool,
) -> LockDecision {
    match age {
        Some(age) if age >= stale_after && !takeover_used => LockDecision::Takeover { age },
        _ => LockDecision::Wait { age },
    }
}

/// 从锁文件的 mtime 与当前时刻算年龄；mtime 在未来（时钟回拨）按 0 处理。
pub fn lock_age(mtime: SystemTime, now: SystemTime) -> Option<Duration> {
    let base = mtime.min(now);
    now.duration_since(base).ok()
}

/// 锁竞争策略。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LockPolicy {
    pub stale_after: Duration,
    pub wait_attempts: u32,
    pub wait_interval: Duration,
}

impl Default for LockPolicy {
    fn default() -> Self {
        Self {
            stale_after: DEFAULT_LOCK_STALE_AFTER,
            wait_attempts: DEFAULT_LOCK_WAIT_ATTEMPTS,
            wait_interval: DEFAULT_LOCK_WAIT_INTERVAL,
        }
    }
}

/// 抢锁失败的两种形状，调用方要能给它们不同的口径：把磁盘权限问题报成"有人在写"，
/// 与把孤儿锁报成"有人在写"是同一类谎言（V11 §40 D1）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockError {
    /// 锁由活的持有者占着，等待预算用尽。
    Contended(String),
    /// 抢锁动作本身失败（权限、只读目录、磁盘）。
    Io(String),
}

impl LockError {
    pub fn message(&self) -> &str {
        match self {
            Self::Contended(message) | Self::Io(message) => message,
        }
    }
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for LockError {}

/// 已持有的文件锁；`Drop` 只在锁文件仍然是自己那一份时删除它，进程被杀时由下一次竞争
/// 的年龄判据接管。
#[derive(Debug)]
pub struct FileLock {
    path: PathBuf,
    token: String,
}

impl FileLock {
    /// 以默认策略抢锁。
    pub fn acquire(path: impl Into<PathBuf>) -> Result<Self, LockError> {
        Self::acquire_with(path, LockPolicy::default())
    }

    /// 抢锁：`create_new` 独占创建，失败后有界等待并按年龄接管孤儿锁。
    pub fn acquire_with(path: impl Into<PathBuf>, policy: LockPolicy) -> Result<Self, LockError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                LockError::Io(format!("创建锁目录 {} 失败: {error}", parent.display()))
            })?;
        }
        let mut last_age: Option<Duration> = None;
        let mut takeover_used = false;
        for _attempt in 0..policy.wait_attempts.max(1) {
            match create_lock_file(&path) {
                LockCreate::Created(token) => return Ok(Self { path, token }),
                LockCreate::Busy => {
                    // 年龄只走判据交出的那一格：`last_age` 不再自己留一份，否则"报出去的
                    // 年龄"与"判据看到的年龄"会是同一件事的两种写法（V11 R7-9）。
                    match decide_lock(age_of(&path), policy.stale_after, takeover_used) {
                        LockDecision::Takeover { age } => {
                            last_age = Some(age);
                            // 只接管一次：删掉孤儿锁后重新抢，若又被别人抢走就按新鲜锁继续等，
                            // 从而把"两个写者互相删对方刚建的锁"排除在判据之外。
                            takeover_used = true;
                            // 动手前复读年龄：观察到孤儿与删除之间可能已经有新的活持有者登记
                            // 进来（它的 mtime 是新的），那一刻它就不再是孤儿了。
                            match age_of(&path) {
                                Some(age) if age >= policy.stale_after => {
                                    let _ = std::fs::remove_file(&path);
                                }
                                _ => {}
                            }
                        }
                        LockDecision::Wait { age } => {
                            last_age = age;
                            if policy.wait_interval > Duration::ZERO {
                                std::thread::sleep(policy.wait_interval);
                            }
                        }
                    }
                }
                LockCreate::Failed(error) => {
                    return Err(LockError::Io(format!(
                        "创建锁文件 {} 失败: {error}",
                        path.display()
                    )))
                }
            }
        }
        Err(LockError::Contended(lock_contention_message(
            &path, last_age, &policy,
        )))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // 只删自己那一把（V11 K3）。原形状是无条件删路径上的文件：一次年龄接管把锁交给
        // 新持有者之后，旧持有者走到收尾时会把**新持有者的锁**删掉，于是第三个写者能与
        // 第二个写者同时进入临界区——D1 打开的接管口子，危害正好落在接管之后。
        // 读不到文件按"不是我的"处理：不删总是安全的，误删不是。
        let still_ours = std::fs::read_to_string(&self.path)
            .map(|text| text.trim() == self.token.trim())
            .unwrap_or(false);
        if still_ours {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// 锁被占用的口径：点名锁文件、最后看到的年龄、阈值与等待预算，并交代接管条件。
/// 这一句要能让人不查代码就判断"是别人在写"还是"上一次崩了"。
pub fn lock_contention_message(path: &Path, age: Option<Duration>, policy: &LockPolicy) -> String {
    let holder = std::fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty());
    let holder_note = match holder.as_deref() {
        Some(value) => format!("holder={value}"),
        None => "无持有者登记".to_string(),
    };
    match age {
        Some(age) if age >= policy.stale_after => format!(
            "锁文件 {} 由 {holder_note} 持有已超过 {} 秒却仍未接管成功（可能有进程在同一时刻\
             抢下又崩溃）；请确认没有进程正在写入，再删除该文件",
            path.display(),
            age.as_secs()
        ),
        Some(age) => format!(
            "锁文件 {} 由 {holder_note} 在 {} 毫秒前创建，尚未达到 {} 秒的孤儿判定，\
             已等待 {} 次；请让并发写者完成后重试",
            path.display(),
            age.as_millis(),
            policy.stale_after.as_secs(),
            policy.wait_attempts
        ),
        None => format!(
            "锁文件 {} 正在被创建或删除，无法读出年龄，已等待 {} 次；请稍后重试",
            path.display(),
            policy.wait_attempts
        ),
    }
}

enum LockCreate {
    /// 抢到锁，携带写进锁文件的所有权令牌。
    Created(String),
    Busy,
    Failed(String),
}

/// 一次抢锁的所有权令牌：PID + 进程内单调计数 + 纳秒时钟。
///
/// 只有 PID 不够——同一进程内两次抢同一把锁（第一次的锁已被接管后重新抢）会写出同一份
/// 内容，谁的 Drop 都能删掉谁的锁。计数与纳秒把同进程内的两次持有也分开了。
fn lock_token() -> String {
    static LOCK_TOKEN_SERIAL: AtomicU64 = AtomicU64::new(0);
    let serial = LOCK_TOKEN_SERIAL.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!("{}-{}-{}", std::process::id(), serial, nanos)
}

fn create_lock_file(path: &Path) -> LockCreate {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let token = lock_token();
    match options.open(path) {
        Ok(mut file) => {
            // 令牌只是诊断与所有权字段：写不进去也不能把已经抢到的锁丢掉——那时宁可留一把
            // 无主可认的锁（接管后 Drop 认不出、因而不删），也不能报"没锁住"。
            let _ = file.write_all(token.as_bytes());
            LockCreate::Created(token)
        }
        // Windows 在删除窗口内可能把"锁正在被释放"报成 PermissionDenied 而非 AlreadyExists。
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::AlreadyExists | ErrorKind::PermissionDenied
            ) =>
        {
            LockCreate::Busy
        }
        Err(error) => LockCreate::Failed(error.to_string()),
    }
}

fn age_of(path: &Path) -> Option<Duration> {
    let mtime = std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()?;
    lock_age(mtime, SystemTime::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "qx-file-lock-{name}-{}-{}",
            std::process::id(),
            SystemTime::UNIX_EPOCH
                .elapsed()
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn touch_with_age(path: &Path, age: Duration) {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .expect("create lock")
            .set_modified(SystemTime::now() - age)
            .expect("backdate lock");
    }

    #[test]
    fn decision_is_pure_and_bounded() {
        let stale = Duration::from_secs(30);
        assert_eq!(
            decide_lock(Some(stale), stale, false),
            LockDecision::Takeover { age: stale }
        );
        assert_eq!(
            decide_lock(Some(stale - Duration::from_millis(1)), stale, false),
            LockDecision::Wait {
                age: Some(stale - Duration::from_millis(1))
            }
        );
        assert_eq!(
            decide_lock(None, stale, false),
            LockDecision::Wait { age: None },
            "读不到年龄必须等，不能抢"
        );
    }

    /// "同一场竞争只接管一次"必须是纯判据能答出来的事：删过一次孤儿锁之后再见到过期锁，
    /// 只能等。缺了这条，两个写者会互相删对方刚建的锁，而这一形状只在进程被杀之后才看得见。
    #[test]
    fn second_stale_sighting_in_one_competition_waits_instead_of_deleting() {
        let stale = Duration::from_secs(3600);
        assert_eq!(
            decide_lock(Some(stale), Duration::from_secs(30), true),
            LockDecision::Wait { age: Some(stale) },
            "已经接管过一次就不得再删别人的锁"
        );
        assert_eq!(
            decide_lock(Some(stale), Duration::from_secs(30), false),
            LockDecision::Takeover { age: stale },
            "第一次见到孤儿锁必须给出接管，否则崩溃一次即永久阻断"
        );
    }

    #[test]
    fn clock_skew_never_yields_negative_age() {
        let now = SystemTime::now();
        assert_eq!(
            lock_age(now + Duration::from_secs(5), now),
            Some(Duration::ZERO)
        );
        assert_eq!(lock_age(now, now), Some(Duration::ZERO));
    }

    #[test]
    fn acquire_creates_then_removes_on_drop() {
        let dir = temp_dir("drop");
        let path = dir.join("x.lock");
        let held = FileLock::acquire(&path).expect("acquire");
        assert!(path.exists());
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with(&format!("{}-", std::process::id())),
            "锁文件要登记本进程的所有权令牌"
        );
        assert_eq!(held.path(), path.as_path());
        drop(held);
        assert!(!path.exists(), "Drop 必须删除锁文件");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn orphan_lock_is_taken_over() {
        let dir = temp_dir("takeover");
        let path = dir.join("orphan.lock");
        touch_with_age(&path, Duration::from_secs(3600));
        let policy = LockPolicy {
            stale_after: Duration::from_millis(500),
            wait_attempts: 4,
            wait_interval: Duration::ZERO,
        };
        let held = FileLock::acquire_with(&path, policy).expect("stale lock must be taken over");
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with(&format!("{}-", std::process::id())),
            "接管后锁必须登记新持有者"
        );
        drop(held);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fresh_lock_fails_boundedly_and_names_the_holder() {
        let dir = temp_dir("fresh");
        let path = dir.join("fresh.lock");
        std::fs::write(&path, "999999").expect("seed lock");
        let policy = LockPolicy {
            stale_after: Duration::from_secs(30),
            wait_attempts: 3,
            wait_interval: Duration::from_millis(10),
        };
        let started = std::time::Instant::now();
        let error =
            FileLock::acquire_with(&path, policy).expect_err("fresh lock must not be stolen");
        let elapsed = started.elapsed();
        assert!(
            matches!(error, LockError::Contended(_)),
            "新鲜锁失败必须报成竞争: {error:?}"
        );
        let message = error.message();
        assert!(message.contains(&path.display().to_string()), "{message}");
        assert!(message.contains("holder=999999"), "{message}");
        assert!(message.contains("尚未达到"), "{message}");
        assert!(path.exists(), "等待预算用尽不得删掉别人的锁");
        // 两条界一起才叫"有界等待"：一看到 Busy 就退出的实现同样交出 Contended，只是它把
        // 并发写者正常干活的那几十毫秒当成了竞争失败。
        assert!(
            elapsed >= Duration::from_millis(10) && elapsed < Duration::from_secs(2),
            "等待必须真的等过预算、又必须在预算用尽时收场，实际 {elapsed:?}: {message}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 占用方在预算内收尾时，等待方要拿到锁而不是把这次写失败上抛。
    /// 上面那颗只答得出"抢不到时长什么样"，答不出"等是有回报的"。
    #[test]
    fn a_holder_that_releases_inside_the_budget_lets_the_waiter_in() {
        let dir = temp_dir("release");
        let path = dir.join("brief.lock");
        let held = FileLock::acquire(&path).expect("acquire");
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            drop(held);
        });
        let waiter = FileLock::acquire_with(
            &path,
            LockPolicy {
                stale_after: Duration::from_secs(30),
                wait_attempts: 2_000,
                wait_interval: Duration::from_millis(2),
            },
        );
        releaser.join().expect("releaser panicked");
        let waiter = waiter.expect("占用方在预算内释放时，等待必须拿到锁而不是把写失败上抛");
        assert!(
            std::fs::read_to_string(&path)
                .expect("waiter must own the file")
                .starts_with(&format!("{}-", std::process::id())),
            "等到的那把锁要登记自己为持有者"
        );
        drop(waiter);
        assert!(!path.exists(), "等待得到的锁同样由 Drop 收回");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drop_does_not_delete_a_lock_somebody_else_holds_now() {
        // V11 K3 的反例：A 持锁 → 一次接管把锁交给 B（这里直接把 B 登记的内容写进同一
        // 路径复现接管之后的状态）→ A 收尾。无条件删除会把 B 的锁删掉，第三个写者于是
        // 能与 B 同时进临界区。
        let dir = temp_dir("foreign-drop");
        let path = dir.join("theft.lock");
        let held = FileLock::acquire(&path).expect("acquire");
        let forged = "999999-0-1700000000000000000";
        std::fs::write(&path, forged).expect("simulate takeover");
        drop(held);
        assert!(
            path.exists(),
            "锁已经换了持有者，旧持有者的 Drop 不得把它删掉"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), forged);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn takeover_happens_at_most_once_per_competition() {
        let dir = temp_dir("single-takeover");
        let path = dir.join("once.lock");
        touch_with_age(&path, Duration::from_secs(3600));
        let policy = LockPolicy {
            stale_after: Duration::from_millis(500),
            wait_attempts: 8,
            wait_interval: Duration::ZERO,
        };
        let held = FileLock::acquire_with(&path, policy).expect("first takeover acquires");
        // 持锁期间再起一把：必须按新鲜锁失败，而不是互相删锁。
        let error = FileLock::acquire_with(&path, policy).expect_err("second must not steal");
        assert!(matches!(error, LockError::Contended(_)), "{error:?}");
        assert!(error.message().contains("尚未达到"), "{error}");
        assert!(path.exists(), "被抢掉的活锁必须还在");
        drop(held);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn contention_message_covers_all_three_observations() {
        let dir = temp_dir("message");
        let path = dir.join("missing.lock");
        let policy = LockPolicy::default();
        let none = lock_contention_message(&path, None, &policy);
        assert!(none.contains("无法读出年龄"), "{none}");
        let young = lock_contention_message(&path, Some(Duration::from_secs(1)), &policy);
        assert!(young.contains("无持有者登记"), "{young}");
        assert!(!young.contains("由 ，"), "文案不得出现悬空介词: {young}");
        let old = lock_contention_message(&path, Some(Duration::from_secs(99)), &policy);
        assert!(old.contains("请确认没有进程正在写入"), "{old}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
