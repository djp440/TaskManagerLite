//! 采样线程：按采样间隔生成快照，交给界面线程。窗口不可见时默认停止采样；开启“后台继续采样”后只采系统指标；用户暂停时不采样。

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::Duration;

use tml_core::{Sample, Sampler, Scope};

pub enum Command {
    /// 立即补采一次（例如结束进程之后）。
    Wake,
    /// `None` 表示用户暂停。
    Interval(Option<Duration>),
    Visible(bool),
    /// 后台继续采样开关（窗口不可见时是否继续采系统指标）。
    Background(bool),
    /// 迷你模式（只显示系统指标，不需要进程快照）。
    Mini(bool),
    /// 是否每次采样都枚举窗口、找出应用进程（分组视图需要）。
    Apps(bool),
    /// 是否用 ETW 统计进程磁盘速率（设置项 `accurate_disk_io`；需要管理员权限）。
    AccurateDisk(bool),
}

/// 当前应当怎样采样。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// 系统指标 + 进程快照。
    Full,
    /// 只采系统指标（迷你模式，或窗口不可见但开启了后台采样）。
    MetricsOnly,
    /// 不采样。
    Idle,
}

/// 可见性、是否迷你模式、后台采样开关、是否被用户暂停 → 采样策略。
/// `visible` 指当前模式的窗口（完整窗口或迷你窗口）是否可见。
pub fn plan(visible: bool, mini: bool, background: bool, paused: bool) -> Plan {
    match (paused, visible, mini, background) {
        (true, ..) => Plan::Idle,
        (false, true, false, _) => Plan::Full,
        (false, true, true, _) => Plan::MetricsOnly,
        (false, false, _, true) => Plan::MetricsOnly,
        (false, false, _, false) => Plan::Idle,
    }
}

#[derive(Clone)]
pub struct SamplerHandle {
    tx: Sender<Command>,
}

impl SamplerHandle {
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }
}

/// `track_apps`：一开始就枚举应用窗口（设置里的视图模式是分组时，第一个快照就要带上）。
/// `accurate_disk`：设置里开启了 ETW 磁盘速率。
/// `background`：设置里开启了后台继续采样（窗口不可见时只采系统指标）。
/// `deliver` 在采样线程上调用，负责把快照投递到界面线程。
pub fn spawn(
    track_apps: bool,
    accurate_disk: bool,
    background: bool,
    deliver: impl Fn(Sample) + Send + 'static,
) -> SamplerHandle {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("sampler".into())
        .spawn(move || {
            let mut sampler = Sampler::default();
            sampler.track_apps(track_apps);
            sampler.set_accurate_disk(accurate_disk);
            let mut interval = Some(Duration::from_secs(1));
            let mut visible = true;
            let mut background = background;
            let mut mini = false;
            // 暂停时收到 Wake 也补采一次，让结束进程的结果能反映到列表上。
            let mut forced = false;
            loop {
                let plan = plan(visible, mini, background, interval.is_none());
                let active = plan != Plan::Idle;
                match plan {
                    Plan::Full => deliver(sampler.sample()),
                    Plan::MetricsOnly => deliver(sampler.sample_scope(Scope::Metrics)),
                    // 迷你模式只显示系统指标，补采也不需要进程快照。
                    Plan::Idle if forced && visible && mini => deliver(sampler.sample_scope(Scope::Metrics)),
                    Plan::Idle if forced && visible => deliver(sampler.sample()),
                    Plan::Idle => {}
                }
                forced = false;
                let cmd = match (active, interval) {
                    (true, Some(d)) => match rx.recv_timeout(d) {
                        Ok(c) => c,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => return,
                    },
                    _ => match rx.recv() {
                        Ok(c) => c,
                        Err(_) => return,
                    },
                };
                // 收到任何命令都会立即补采一次（若处于活动状态），并重新开始计时。
                match cmd {
                    Command::Wake => forced = true,
                    Command::Interval(d) => interval = d,
                    Command::Visible(v) => visible = v,
                    Command::Background(on) => background = on,
                    Command::Mini(on) => mini = on,
                    Command::AccurateDisk(on) => sampler.set_accurate_disk(on),
                    Command::Apps(on) => {
                        sampler.track_apps(on);
                        // 暂停时也补采一次，让刚切换的视图有数据。
                        forced = on;
                    }
                }
            }
        })
        .expect("无法创建采样线程");
    SamplerHandle { tx }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visible_samples_fully_regardless_of_background() {
        assert_eq!(plan(true, false, false, false), Plan::Full);
        assert_eq!(plan(true, false, true, false), Plan::Full);
    }

    #[test]
    fn hidden_pauses_unless_background_is_on() {
        assert_eq!(plan(false, false, false, false), Plan::Idle);
        assert_eq!(plan(false, false, true, false), Plan::MetricsOnly);
    }

    #[test]
    fn user_pause_wins_over_everything() {
        for visible in [false, true] {
            for mini in [false, true] {
                for background in [false, true] {
                    assert_eq!(plan(visible, mini, background, true), Plan::Idle);
                }
            }
        }
    }

    #[test]
    fn mini_mode_samples_metrics_only() {
        assert_eq!(plan(true, true, false, false), Plan::MetricsOnly);
        // 迷你窗口隐藏到托盘时与完整窗口隐藏相同。
        assert_eq!(plan(false, true, false, false), Plan::Idle);
        assert_eq!(plan(false, true, true, false), Plan::MetricsOnly);
    }
}
