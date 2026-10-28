//! 采样线程：按采样间隔生成快照，交给界面线程；窗口不可见或暂停时停止采样。

use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::Duration;

use tml_core::{Sample, Sampler};

pub enum Command {
    /// 立即补采一次（例如结束进程之后）。
    Wake,
    /// `None` 表示用户暂停。
    Interval(Option<Duration>),
    Visible(bool),
    /// 是否每次采样都枚举窗口、找出应用进程（分组视图需要）。
    Apps(bool),
    /// 是否用 ETW 统计进程磁盘速率（设置项 `accurate_disk_io`；需要管理员权限）。
    /// 运行中切换的入口，设置页（#16）接入前暂无发送方。
    #[allow(dead_code)]
    AccurateDisk(bool),
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
/// `deliver` 在采样线程上调用，负责把快照投递到界面线程。
pub fn spawn(track_apps: bool, accurate_disk: bool, deliver: impl Fn(Sample) + Send + 'static) -> SamplerHandle {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("sampler".into())
        .spawn(move || {
            let mut sampler = Sampler::default();
            sampler.track_apps(track_apps);
            sampler.set_accurate_disk(accurate_disk);
            let mut interval = Some(Duration::from_secs(1));
            let mut visible = true;
            // 暂停时收到 Wake 也补采一次，让结束进程的结果能反映到列表上。
            let mut forced = false;
            loop {
                let active = visible && interval.is_some();
                if active || (forced && visible) {
                    deliver(sampler.sample());
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
