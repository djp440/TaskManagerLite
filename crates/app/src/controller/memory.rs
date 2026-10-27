//! 内存详情页的数据整形：把采集到的内存状态与硬件信息变成界面上的文字与组合条比例。
//!
//! `describe` 是纯函数，可直接单测；`view` 只做到界面结构体的转换。

use tml_core::{MemoryHardware, MemoryStatus};

use crate::{MemoryView, StatItem};

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
const MIB: f64 = 1024.0 * 1024.0;
/// 没有数据（硬件信息尚未读取、固件不提供）时的占位。
const UNKNOWN: &str = "—";

/// 容量文字：不足 1 GB 用 MB（取整），否则用 GB（一位小数），与原生任务管理器一致。
pub fn size(bytes: u64) -> String {
    let b = bytes as f64;
    if b < GIB { format!("{:.0} MB", b / MIB) } else { format!("{:.1} GB", b / GIB) }
}

/// 界面需要的全部文字，顺序即显示顺序。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MemoryText {
    /// 标题栏右侧：安装容量与内存类型。
    pub header: String,
    pub stats: Vec<(String, String)>,
    pub info: Vec<(String, String)>,
    /// 内存组合条图例：使用中、已修改、备用、空闲。
    pub composition: Vec<(String, String)>,
    /// 组合条前三段占物理总量的比例（使用中、已修改、备用），余下为空闲。
    pub fractions: [f32; 3],
}

pub fn describe(s: &MemoryStatus, hw: Option<&MemoryHardware>) -> MemoryText {
    let pair = |l: &str, v: String| (l.to_string(), v);
    let stats = vec![
        pair("使用中", size(s.in_use())),
        pair("可用", size(s.available)),
        pair("已提交", format!("{:.1}/{:.1} GB", s.committed as f64 / GIB, s.commit_limit as f64 / GIB)),
        pair("已缓存", size(s.cached())),
        pair("已压缩", size(s.compressed)),
        pair("分页缓冲池", size(s.paged_pool)),
        pair("非分页缓冲池", size(s.nonpaged_pool)),
    ];

    let installed = hw.map(|h| h.installed).filter(|&n| n > 0);
    let unknown = || UNKNOWN.to_string();
    let info = vec![
        pair("总容量", size(installed.unwrap_or(s.total))),
        pair("速度", hw.and_then(|h| h.speed_mts).map_or_else(unknown, |v| format!("{v} MT/s"))),
        pair(
            "已使用的插槽",
            hw.filter(|h| h.slots_total > 0).map_or_else(unknown, |h| format!("{}/{}", h.slots_used, h.slots_total)),
        ),
        pair("外形规格", hw.and_then(|h| h.form_factor).map_or_else(unknown, str::to_string)),
        pair("为硬件保留的内存", installed.map_or_else(unknown, |n| size(n.saturating_sub(s.total)))),
    ];

    let header = match hw {
        Some(h) if h.installed > 0 => match h.memory_type {
            Some(t) => format!("{} {t}", size(h.installed)),
            None => size(h.installed),
        },
        _ => size(s.total),
    };

    let composition = vec![
        pair("使用中", size(s.in_use())),
        pair("已修改", size(s.modified)),
        pair("备用", size(s.standby)),
        pair("空闲", size(s.free)),
    ];
    let frac = |n: u64| if s.total > 0 { (n as f64 / s.total as f64).clamp(0.0, 1.0) as f32 } else { 0.0 };
    let mut fractions = [frac(s.in_use()), frac(s.modified), frac(s.standby)];
    // 各段之和不超过 1，避免读数的瞬时误差让组合条溢出。
    let sum: f32 = fractions.iter().sum();
    if sum > 1.0 {
        fractions.iter_mut().for_each(|f| *f /= sum);
    }

    MemoryText { header, stats, info, composition, fractions }
}

fn items(v: Vec<(String, String)>) -> slint::ModelRc<StatItem> {
    let rows: Vec<StatItem> = v.into_iter().map(|(l, v)| StatItem { label: l.into(), value: v.into() }).collect();
    slint::ModelRc::new(slint::VecModel::from(rows))
}

pub fn view(text: MemoryText) -> MemoryView {
    MemoryView {
        stats: items(text.stats),
        info: items(text.info),
        composition: items(text.composition),
        in_use: text.fractions[0],
        modified: text.fractions[1],
        standby: text.fractions[2],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const G: u64 = 1024 * 1024 * 1024;

    fn status() -> MemoryStatus {
        MemoryStatus {
            total: 16 * G,
            available: 6 * G,
            free: 2 * G,
            modified: G / 2,
            standby: 3 * G + G / 2,
            compressed: 300 * 1024 * 1024,
            paged_pool: 400 * 1024 * 1024,
            nonpaged_pool: G + G / 10,
            committed: 20 * G,
            commit_limit: 40 * G,
        }
    }

    fn value<'a>(rows: &'a [(String, String)], label: &str) -> &'a str {
        &rows.iter().find(|r| r.0 == label).unwrap().1
    }

    #[test]
    fn size_switches_between_mb_and_gb() {
        assert_eq!(size(177 * 1024 * 1024), "177 MB");
        assert_eq!(size(G), "1.0 GB");
        assert_eq!(size(16 * G + G / 2), "16.5 GB");
        assert_eq!(size(0), "0 MB");
    }

    #[test]
    fn stats_follow_native_semantics() {
        let t = describe(&status(), None);
        assert_eq!(value(&t.stats, "使用中"), "10.0 GB");
        assert_eq!(value(&t.stats, "可用"), "6.0 GB");
        assert_eq!(value(&t.stats, "已提交"), "20.0/40.0 GB");
        // 已缓存 = 备用 + 已修改。
        assert_eq!(value(&t.stats, "已缓存"), "4.0 GB");
        assert_eq!(value(&t.stats, "已压缩"), "300 MB");
        assert_eq!(value(&t.stats, "分页缓冲池"), "400 MB");
        assert_eq!(value(&t.stats, "非分页缓冲池"), "1.1 GB");
    }

    #[test]
    fn info_without_hardware_uses_placeholders() {
        let t = describe(&status(), None);
        assert_eq!(value(&t.info, "总容量"), "16.0 GB");
        for k in ["速度", "已使用的插槽", "外形规格", "为硬件保留的内存"] {
            assert_eq!(value(&t.info, k), UNKNOWN, "{k}");
        }
        assert_eq!(t.header, "16.0 GB");
    }

    #[test]
    fn info_with_hardware() {
        let hw = MemoryHardware {
            installed: 16 * G,
            speed_mts: Some(4800),
            slots_used: 2,
            slots_total: 4,
            form_factor: Some("SODIMM"),
            memory_type: Some("DDR5"),
        };
        let mut s = status();
        s.total = 16 * G - 256 * 1024 * 1024;
        let t = describe(&s, Some(&hw));
        assert_eq!(value(&t.info, "总容量"), "16.0 GB");
        assert_eq!(value(&t.info, "速度"), "4800 MT/s");
        assert_eq!(value(&t.info, "已使用的插槽"), "2/4");
        assert_eq!(value(&t.info, "外形规格"), "SODIMM");
        assert_eq!(value(&t.info, "为硬件保留的内存"), "256 MB");
        assert_eq!(t.header, "16.0 GB DDR5");
    }

    #[test]
    fn unknown_slot_total_shows_placeholder() {
        let hw = MemoryHardware { installed: 8 * G, slots_used: 0, slots_total: 0, ..Default::default() };
        let t = describe(&status(), Some(&hw));
        assert_eq!(value(&t.info, "已使用的插槽"), UNKNOWN);
    }

    #[test]
    fn composition_fractions_stay_within_one() {
        let t = describe(&status(), None);
        assert!((t.fractions[0] - 10.0 / 16.0).abs() < 1e-6);
        assert!((t.fractions[1] - 0.5 / 16.0).abs() < 1e-6);
        assert!(t.fractions.iter().sum::<f32>() <= 1.0);
        assert_eq!(value(&t.composition, "空闲"), "2.0 GB");

        // 读数瞬时不一致导致各段之和超过总量时按比例压缩。
        let mut s = status();
        s.modified = 8 * G;
        s.standby = 8 * G;
        let t = describe(&s, None);
        assert!(t.fractions.iter().sum::<f32>() <= 1.0 + 1e-6);
        assert_eq!(describe(&MemoryStatus::default(), None).fractions, [0.0; 3]);
    }
}
