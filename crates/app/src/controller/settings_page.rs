//! 设置页（#16）：`SettingsData` 的初始值、回调，以及开机启动与接管任务管理器的状态读取。
//!
//! 开机启动与接管任务管理器的开关状态以注册表为准，每次进入设置页时重新读取；其余开关以设置文件为准，改动后立即保存。

use std::path::Path;
use std::rc::Rc;

use slint::ComponentHandle;
use tml_core::{Autostart, AutostartKey, IfeoError, Registration};

use super::Controller;
use crate::sampling::Command;
use crate::{AppWindow, SettingsData};

/// 开机启动的状态说明。
fn autostart_note(status: &std::io::Result<Autostart>) -> String {
    match status {
        Ok(Autostart::Off | Autostart::On) => String::new(),
        Ok(Autostart::OtherPath(value)) => {
            format!("已登记的是另一个位置的程序（{value}），开启后改为当前程序")
        }
        Ok(Autostart::DisabledBySystem) => "已在 Windows 设置的“启动应用”中被禁用，重新开启即可恢复".into(),
        Err(e) => format!("无法读取开机启动状态：{e}"),
    }
}

/// 接管任务管理器的状态说明与是否为错误。`wanted` 是设置里记录的意愿。
fn taskmgr_note(status: &Result<Registration, IfeoError>, wanted: bool) -> (String, bool) {
    match status {
        Ok(Registration::ThisExe) => (String::new(), false),
        Ok(Registration::NotRegistered) if wanted => ("登记已被移除，重新开启即可恢复".into(), false),
        Ok(Registration::NotRegistered) => (String::new(), false),
        Ok(Registration::OtherExe { debugger, exists: true }) => (format!("已被其他程序接管：{debugger}"), false),
        Ok(Registration::OtherExe { debugger, exists: false }) => {
            (format!("登记的程序已不存在（{debugger}），开启后改为本程序"), false)
        }
        Err(e) => (format!("无法读取登记状态：{e}"), true),
    }
}

fn exe() -> std::io::Result<std::path::PathBuf> {
    std::env::current_exe()
}

impl Controller {
    pub(super) fn bind_settings(self: &Rc<Self>, ui: &AppWindow) {
        let data = ui.global::<SettingsData>();
        {
            let s = self.settings.borrow();
            data.set_close_to_tray(s.close_to_tray);
            data.set_theme(s.theme);
            data.set_background_sampling(s.background_sampling);
            data.set_accurate_disk(s.accurate_disk_io);
            data.set_replace_taskmgr(s.replace_taskmgr);
            // 0（跟随系统）是界面的默认状态，无需设置。
            if s.theme != 0 {
                ui.invoke_set_theme(s.theme);
            }
        }
        data.set_settings_path(self.store.as_ref().map(|s| s.path().display().to_string()).unwrap_or_default().into());

        let c = Rc::downgrade(self);
        data.on_refresh(move || {
            if let Some(c) = c.upgrade() {
                c.refresh_settings_page();
            }
        });
        let c = Rc::downgrade(self);
        data.on_autostart_toggled(move |on| {
            if let Some(c) = c.upgrade() {
                c.set_autostart(on);
            }
        });
        let c = Rc::downgrade(self);
        data.on_close_to_tray_toggled(move |on| {
            if let Some(c) = c.upgrade() {
                c.set_close_to_tray(on);
                c.save_settings();
            }
        });
        let c = Rc::downgrade(self);
        data.on_theme_selected(move |mode| {
            if let Some(c) = c.upgrade() {
                c.set_theme(mode);
            }
        });
        let c = Rc::downgrade(self);
        data.on_background_sampling_toggled(move |on| {
            if let Some(c) = c.upgrade() {
                c.set_background_sampling(on);
                c.save_settings();
            }
        });
        let c = Rc::downgrade(self);
        data.on_accurate_disk_toggled(move |on| {
            if let Some(c) = c.upgrade() {
                c.settings.borrow_mut().accurate_disk_io = on;
                c.sampler.send(Command::AccurateDisk(on));
                c.log.borrow_mut().line(format!("精确磁盘统计 意愿={on}"));
                c.save_settings();
            }
        });
        let c = Rc::downgrade(self);
        data.on_replace_taskmgr_toggled(move |on| {
            if let Some(c) = c.upgrade() {
                c.set_replace_taskmgr(on);
            }
        });
        let c = Rc::downgrade(self);
        data.on_open_settings_folder(move || {
            if let Some(c) = c.upgrade() {
                c.open_settings_folder();
            }
        });
    }

    /// 进入设置页时重新读取以注册表为准的两项（用户可能在系统设置或别的工具里改过）。
    pub fn refresh_settings_page(&self) {
        self.refresh_autostart(None);
        if !self.taskmgr_busy() {
            self.refresh_taskmgr(None);
        }
    }

    fn settings_data(&self, f: impl FnOnce(SettingsData<'_>)) {
        if let Some(ui) = self.ui.upgrade() {
            f(ui.global::<SettingsData>());
        }
    }

    /// 读取开机启动状态写回界面；`error` 是刚才修改失败的原因，优先显示。
    fn refresh_autostart(&self, error: Option<String>) {
        let status = exe().and_then(|exe| AutostartKey::from_env().status(&exe));
        let note = error.unwrap_or_else(|| autostart_note(&status));
        let on = status.as_ref().is_ok_and(Autostart::is_on);
        self.settings_data(|d| {
            d.set_autostart(on);
            d.set_autostart_note(note.into());
        });
    }

    fn set_autostart(&self, on: bool) {
        let result = exe().and_then(|exe| AutostartKey::from_env().set(&exe, on));
        self.log.borrow_mut().line(format!("开机启动 设为={on} 结果={result:?}"));
        self.refresh_autostart(result.err().map(|e| format!("无法修改开机启动：{e}")));
    }

    fn set_theme(&self, mode: i32) {
        let mode = mode.clamp(0, 2);
        self.settings.borrow_mut().theme = mode;
        if let Some(ui) = self.ui.upgrade() {
            ui.invoke_set_theme(mode);
        }
        if let Some(mini) = self.mini.get() {
            mini.invoke_set_theme(mode);
        }
        self.save_settings();
    }

    fn taskmgr_busy(&self) -> bool {
        self.ui.upgrade().is_some_and(|ui| ui.global::<SettingsData>().get_taskmgr_busy())
    }

    /// 读取 IFEO 登记状态写回界面；`error` 是刚才修改失败的原因，优先显示。
    fn refresh_taskmgr(&self, error: Option<IfeoError>) {
        let status = tml_core::Ifeo::from_env()
            .and_then(|ifeo| exe().map_err(|e| IfeoError::Other(e.to_string())).and_then(|exe| ifeo.status(&exe)));
        let wanted = self.settings.borrow().replace_taskmgr;
        let (note, is_error) = match error {
            // 核心库的说明提到命令行的 --force，界面上换成不含它的说法。
            Some(IfeoError::UnsafeLocation(why)) => (
                format!(
                    "程序所在位置可被其他用户改写（{why}），为安全起见不予登记。请把程序移到 Program Files 等受保护的目录后再试"
                ),
                true,
            ),
            Some(e) => (e.to_string(), !matches!(e, IfeoError::Cancelled)),
            None => taskmgr_note(&status, wanted),
        };
        let on = matches!(status, Ok(Registration::ThisExe));
        self.settings_data(|d| {
            d.set_replace_taskmgr(on);
            d.set_taskmgr_note(note.into());
            d.set_taskmgr_error(is_error);
        });
    }

    /// 登记或撤销可能要弹出 UAC 并等待提权子进程，放到后台线程；结束后以注册表的实际状态为准。
    fn set_replace_taskmgr(&self, on: bool) {
        if self.taskmgr_busy() {
            return;
        }
        self.settings_data(|d| d.set_taskmgr_busy(true));
        self.log.borrow_mut().line(format!("接管任务管理器 设为={on}"));
        let spawned = std::thread::Builder::new().name("taskmgr-ifeo".into()).spawn(move || {
            let result = tml_core::set_replace_taskmgr(on, false);
            let _ = slint::invoke_from_event_loop(move || {
                let controller = crate::CONTROLLER.with(|c| c.borrow().clone());
                if let Some(c) = controller {
                    c.finish_replace_taskmgr(on, result);
                }
            });
        });
        if let Err(e) = spawned {
            self.finish_replace_taskmgr(on, Err(IfeoError::Other(format!("无法创建后台线程：{e}"))));
        }
    }

    fn finish_replace_taskmgr(&self, on: bool, result: Result<(), IfeoError>) {
        self.log.borrow_mut().line(format!("接管任务管理器 设为={on} 结果={result:?}"));
        self.settings_data(|d| d.set_taskmgr_busy(false));
        // 成功时记下新的意愿；失败（含取消）时保持原意愿，界面开关回到注册表的实际状态。
        if result.is_ok() {
            self.settings.borrow_mut().replace_taskmgr = on;
            self.save_settings();
        }
        self.refresh_taskmgr(result.err());
    }

    fn open_settings_folder(&self) {
        let Some(store) = &self.store else { return };
        // 先保存一次，确保文件存在、资源管理器能选中它。
        self.save_settings();
        let path: &Path = store.path();
        let result =
            tml_core::reveal_in_explorer(path).or_else(|_| path.parent().map_or(Ok(()), tml_core::reveal_in_explorer));
        if let Err(e) = result {
            self.log.borrow_mut().line(format!("打开设置文件夹失败 {e:?}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 开机启动说明() {
        assert_eq!(autostart_note(&Ok(Autostart::On)), "");
        assert_eq!(autostart_note(&Ok(Autostart::Off)), "");
        assert!(autostart_note(&Ok(Autostart::DisabledBySystem)).contains("启动应用"));
        assert!(autostart_note(&Ok(Autostart::OtherPath(r"D:\old.exe".into()))).contains(r"D:\old.exe"));
    }

    #[test]
    fn 接管说明() {
        assert_eq!(taskmgr_note(&Ok(Registration::ThisExe), true), (String::new(), false));
        assert_eq!(taskmgr_note(&Ok(Registration::NotRegistered), false), (String::new(), false));
        assert!(taskmgr_note(&Ok(Registration::NotRegistered), true).0.contains("移除"));
        let other = Registration::OtherExe { debugger: "x.exe".into(), exists: true };
        assert!(taskmgr_note(&Ok(other), false).0.contains("其他程序"));
        assert!(taskmgr_note(&Err(IfeoError::AccessDenied), false).1);
    }
}
