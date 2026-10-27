//! 进程右键菜单：右键时按需读取所选进程的路径与命令行，决定菜单项是否可用，并执行菜单动作。
//! 读取不进入采样热路径；拿不到（权限不足、受保护进程）的项置灰。

use std::path::{Path, PathBuf};
use std::rc::Rc;

use slint::ComponentHandle;
use tml_core::ProcessKey;

use super::Controller;
use crate::{AppWindow, ProcessMenu};

const STATUS_PATH_COPIED: i32 = 6;
const STATUS_COMMAND_LINE_COPIED: i32 = 7;
const STATUS_ERROR: i32 = 5;

/// 菜单项的可用性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Availability {
    pub open_location: bool,
    pub copy_path: bool,
    pub copy_command_line: bool,
    pub properties: bool,
}

/// 由读到的路径与命令行得出各项是否可用：拿不到路径时与路径相关的项置灰，
/// 命令行为空或读不到时“复制命令行”置灰。
pub fn availability(path: Option<&Path>, command_line: Option<&str>) -> Availability {
    let has_path = path.is_some();
    Availability {
        open_location: has_path,
        copy_path: has_path,
        copy_command_line: command_line.is_some_and(|c| !c.is_empty()),
        properties: has_path,
    }
}

/// 菜单动作失败时在状态栏显示的说明（前面由界面加上“操作失败：”）。
fn failure(action: &str, reason: &str) -> String {
    format!("{action}（{reason}）")
}

impl Controller {
    pub(super) fn bind_process_menu(self: &Rc<Self>, ui: &AppWindow) {
        let menu = ui.global::<ProcessMenu>();
        let c = Rc::downgrade(self);
        let with = move |f: fn(&Controller)| {
            let c = c.clone();
            move || {
                if let Some(c) = c.upgrade() {
                    f(&c);
                }
            }
        };
        menu.on_opening(with(Controller::menu_opening));
        menu.on_open_location(with(Controller::open_location));
        menu.on_copy_path(with(Controller::copy_path));
        menu.on_copy_command_line(with(Controller::copy_command_line));
        menu.on_show_properties(with(Controller::show_properties));
        menu.on_search_online(with(Controller::search_online));
    }

    /// 右键按下、菜单弹出前：读取所选进程的路径与命令行，设置各项是否可用。
    fn menu_opening(&self) {
        let Some(ui) = self.ui.upgrade() else { return };
        let (path, command_line) = match self.selected_process() {
            Some((key, ..)) => (tml_core::image_path_of(key), tml_core::command_line(key)),
            None => (None, None),
        };
        let a = availability(path.as_deref(), command_line.as_deref());
        let menu = ui.global::<ProcessMenu>();
        menu.set_can_open_location(a.open_location);
        menu.set_can_copy_path(a.copy_path);
        menu.set_can_copy_command_line(a.copy_command_line);
        menu.set_can_show_properties(a.properties);
    }

    fn report(&self, kind: i32, arg: String) {
        if let Some(ui) = self.ui.upgrade() {
            ui.set_status_arg(arg.into());
            ui.set_status_kind(kind);
        }
    }

    /// 所选进程的 (身份, 名称, 路径)。进程已退出或读不到路径时给出提示并返回 `None`。
    fn selected_with_path(&self, action: &str) -> Option<(ProcessKey, String, PathBuf)> {
        let (key, name, _) = self.selected_process()?;
        match tml_core::image_path_of(key) {
            Some(path) => Some((key, name, path)),
            None => {
                self.report(STATUS_ERROR, failure(action, &format!("无法读取 {name} 的路径")));
                None
            }
        }
    }

    fn open_location(&self) {
        let Some((_, _, path)) = self.selected_with_path("打开文件位置") else { return };
        if let Err(e) = tml_core::reveal_in_explorer(&path) {
            self.log.borrow_mut().line(format!("打开文件位置失败 {} {e:?}", path.display()));
            self.report(STATUS_ERROR, failure("打开文件位置", &format!("{e:?}")));
        }
    }

    fn copy_path(&self) {
        let Some((_, name, path)) = self.selected_with_path("复制路径") else { return };
        if tml_core::set_clipboard_text(&path.to_string_lossy()) {
            self.report(STATUS_PATH_COPIED, name);
        } else {
            self.report(STATUS_ERROR, failure("复制路径", "剪贴板被占用"));
        }
    }

    fn copy_command_line(&self) {
        let Some((key, name, _)) = self.selected_process() else { return };
        match tml_core::command_line(key) {
            Some(line) if !line.is_empty() => {
                if tml_core::set_clipboard_text(&line) {
                    self.report(STATUS_COMMAND_LINE_COPIED, name);
                } else {
                    self.report(STATUS_ERROR, failure("复制命令行", "剪贴板被占用"));
                }
            }
            _ => self.report(STATUS_ERROR, failure("复制命令行", &format!("无法读取 {name} 的命令行"))),
        }
    }

    fn show_properties(&self) {
        let Some((_, _, path)) = self.selected_with_path("打开属性") else { return };
        if let Err(e) = tml_core::show_properties(&path) {
            self.log.borrow_mut().line(format!("打开属性失败 {} {e:?}", path.display()));
            self.report(STATUS_ERROR, failure("打开属性", &format!("{e:?}")));
        }
    }

    fn search_online(&self) {
        let Some((_, name, _)) = self.selected_process() else { return };
        if let Err(e) = tml_core::search_online(&name) {
            self.log.borrow_mut().line(format!("在线搜索失败 {name} {e:?}"));
            self.report(STATUS_ERROR, failure("在线搜索", &format!("{e:?}")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_process_enables_everything() {
        let a = availability(Some(Path::new(r"C:\Windows\notepad.exe")), Some(r#""notepad.exe" a.txt"#));
        assert_eq!(a, Availability { open_location: true, copy_path: true, copy_command_line: true, properties: true });
    }

    #[test]
    fn protected_process_greys_out_everything_without_error() {
        let a = availability(None, None);
        assert_eq!(a, Availability { open_location: false, copy_path: false, copy_command_line: false, properties: false });
    }

    #[test]
    fn path_and_command_line_are_independent() {
        // 能读到路径但读不到命令行（权限不足）：只有“复制命令行”置灰。
        let a = availability(Some(Path::new(r"C:\x.exe")), None);
        assert!(a.open_location && a.copy_path && a.properties && !a.copy_command_line);
        // 读不到路径但读到命令行：只有“复制命令行”可用。
        let b = availability(None, Some("x"));
        assert!(!b.open_location && !b.copy_path && !b.properties && b.copy_command_line);
        // 空命令行视为不可用。
        assert!(!availability(None, Some("")).copy_command_line);
    }

    #[test]
    fn failure_text_names_the_action_and_reason() {
        assert_eq!(failure("复制路径", "剪贴板被占用"), "复制路径（剪贴板被占用）");
    }
}
