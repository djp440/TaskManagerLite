//! 逻辑处理器网格的布局计算（纯函数，可单测）：给定小图数量与网格区尺寸，得出列数、行数与小图尺寸。
//!
//! 界面（Slint）与曲线生成（Rust）都用同一个 `grid_plan` 得到小图尺寸，所以只要网格区尺寸与小图数量确定，
//! 曲线路径的尺寸就必然等于小图的实际尺寸。
//!
//! 目标：小图不窄于 `MIN_CELL_W`、不矮于 `MIN_CELL_H`；在可选列数里优先让最后一行的空格最少，
//! 空格数相同时选宽高比最接近 `ASPECT` 的。核数多到放不下时不压扁小图，而是保持最小高度、
//! 让网格区自己滚动（`scroll`），并为滚动条让出宽度。

/// 小图之间的间距。
pub const GAP: f32 = 6.0;
/// 小图的最小宽度。
pub const MIN_CELL_W: f32 = 64.0;
/// 小图的最小高度（每行至少约 24px）。
pub const MIN_CELL_H: f32 = 24.0;
/// 小图理想的宽高比。
const ASPECT: f32 = 2.0;
/// 出现竖向滚动条时为它预留的宽度。
pub const SCROLLBAR_W: f32 = 14.0;
/// 小图边框（单侧）的宽度；绘制曲线的区域是小图去掉边框后的内部。
pub const BORDER: f32 = 1.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridPlan {
    pub cols: usize,
    pub rows: usize,
    pub cell_w: f32,
    pub cell_h: f32,
    /// 全部行的总高度；`scroll` 为 false 时等于网格区高度。
    pub content_h: f32,
    /// 网格区放不下全部行（按最小高度），需要滚动。
    pub scroll: bool,
}

impl GridPlan {
    /// 最后一行空着的格数。
    pub fn empty_cells(&self, count: usize) -> usize {
        self.cols * self.rows - count.max(1)
    }

    /// 小图内部（去掉边框）的绘制尺寸，曲线路径按它生成。
    pub fn inner_size(&self) -> (f32, f32) {
        ((self.cell_w - 2.0 * BORDER).max(1.0), (self.cell_h - 2.0 * BORDER).max(1.0))
    }
}

/// 宽度 `w` 内最多放几列（每列不窄于 `MIN_CELL_W`），且不超过小图数量。
fn max_cols(count: usize, w: f32) -> usize {
    (((w + GAP) / (MIN_CELL_W + GAP)).floor() as usize).clamp(1, count)
}

fn plan_with(count: usize, cols: usize, w: f32, h: f32) -> GridPlan {
    let rows = count.div_ceil(cols);
    GridPlan {
        cols,
        rows,
        cell_w: (w - GAP * (cols - 1) as f32) / cols as f32,
        cell_h: (h - GAP * (rows - 1) as f32) / rows as f32,
        content_h: h,
        scroll: false,
    }
}

/// 宽高比与理想值的偏离（对数尺度，宽了窄了一视同仁）。
fn aspect_deviation(p: &GridPlan) -> f32 {
    (p.cell_w / p.cell_h / ASPECT).ln().abs()
}

/// 计算网格布局。`count` 为小图数量，`w`、`h` 为网格区的可用尺寸（逻辑像素）。
pub fn grid_plan(count: usize, w: f32, h: f32) -> GridPlan {
    let count = count.max(1);
    let (w, h) = (w.max(1.0), h.max(1.0));

    // 不滚动：在小图不矮于最小高度的列数里，先选空格最少的，再选宽高比最接近理想值的。
    let best = (1..=max_cols(count, w))
        .map(|cols| plan_with(count, cols, w, h))
        .filter(|p| p.cell_h >= MIN_CELL_H)
        .min_by(|a, b| {
            a.empty_cells(count).cmp(&b.empty_cells(count)).then(aspect_deviation(a).total_cmp(&aspect_deviation(b)))
        });
    if let Some(plan) = best {
        return plan;
    }

    // 放不下：宽度扣掉滚动条，用能放下的最多列把行数压到最少，行高固定为最小高度，网格区滚动。
    // 行数一定时取最少的列数，空格也就最少。
    let w = (w - SCROLLBAR_W).max(1.0);
    let rows = count.div_ceil(max_cols(count, w));
    let cols = (1..=max_cols(count, w)).find(|&c| count.div_ceil(c) == rows).unwrap_or(1);
    GridPlan {
        cols,
        rows,
        cell_w: (w - GAP * (cols - 1) as f32) / cols as f32,
        cell_h: MIN_CELL_H,
        content_h: rows as f32 * MIN_CELL_H + GAP * (rows - 1) as f32,
        scroll: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 默认窗口（1040x700）里网格区的大小。
    const DEFAULT_AREA: (f32, f32) = (724.0, 241.0);
    /// 最小窗口（760x480）里网格区的大小。
    const MIN_AREA: (f32, f32) = (464.0, 80.0);

    fn plan(count: usize, area: (f32, f32)) -> GridPlan {
        grid_plan(count, area.0, area.1)
    }

    /// 能放下全部小图、最后一行不会整行为空、小图不小于最小尺寸。
    fn assert_sane(p: &GridPlan, count: usize) {
        assert!(p.cols * p.rows >= count, "{p:?}");
        assert!(p.cols * (p.rows - 1) < count, "最后一行不应整行为空 {p:?}");
        assert!(p.cell_w >= MIN_CELL_W - 0.01 && p.cell_h >= MIN_CELL_H - 0.01, "{p:?}");
    }

    #[test]
    fn default_window_grids_are_tidy() {
        // (核数, 期望列数, 期望行数)：没有空格，宽高比接近 2:1。
        for (count, cols, rows) in [(12, 4, 3), (16, 4, 4), (24, 6, 4), (64, 8, 8)] {
            let p = plan(count, DEFAULT_AREA);
            assert_eq!((p.cols, p.rows), (cols, rows), "{count} 核：{p:?}");
            assert_eq!(p.empty_cells(count), 0, "{count} 核：{p:?}");
            assert!(!p.scroll && p.content_h == DEFAULT_AREA.1, "{count} 核：{p:?}");
            assert_sane(&p, count);
        }
    }

    #[test]
    fn one_hundred_twenty_eight_cores_scroll_in_the_default_window() {
        // 128 核在 241px 高的网格区里放不下（每行至少 24px）：保持最小高度并滚动，扣掉滚动条后宽 710px，最多 10 列。
        let p = plan(128, DEFAULT_AREA);
        assert!(p.scroll);
        assert_eq!((p.cols, p.rows), (10, 13));
        assert_eq!(p.cell_h, MIN_CELL_H);
        assert_eq!(p.content_h, 13.0 * 24.0 + 12.0 * GAP);
        assert_sane(&p, 128);
    }

    #[test]
    fn big_area_fits_one_hundred_twenty_eight_cores_without_scrolling() {
        let p = grid_plan(128, 1100.0, 600.0);
        assert!(!p.scroll);
        assert_eq!((p.cols, p.rows), (8, 16));
        assert_eq!(p.empty_cells(128), 0);
        assert_sane(&p, 128);
    }

    #[test]
    fn minimum_window_keeps_rows_at_least_min_height() {
        // 12 核：6x2，两行各 37px 高，不滚动。
        let p = plan(12, MIN_AREA);
        assert_eq!((p.cols, p.rows), (6, 2));
        assert!(!p.scroll && (p.cell_h - 37.0).abs() < 0.01, "{p:?}");
        assert_sane(&p, 12);
        // 16、24 核放不下 24px 的行：按最小高度滚动，不把小图压扁。
        for (count, cols, rows) in [(16, 6, 3), (24, 6, 4)] {
            let p = plan(count, MIN_AREA);
            assert!(p.scroll, "{count} 核：{p:?}");
            assert_eq!((p.cols, p.rows), (cols, rows), "{count} 核：{p:?}");
            assert_eq!(p.cell_h, MIN_CELL_H);
            assert_sane(&p, count);
        }
        // 64 核（计划里点名要覆盖）：滚动，内容高度随行数增长。
        let p = plan(64, MIN_AREA);
        assert!(p.scroll);
        assert_eq!((p.cols, p.rows), (6, 11));
        assert_eq!(p.content_h, 11.0 * 24.0 + 10.0 * GAP);
        assert!(p.content_h > MIN_AREA.1);
        assert_sane(&p, 64);
        // 128 核：行数更多，内容更高。
        let q = plan(128, MIN_AREA);
        assert!(q.scroll && q.rows > p.rows && q.content_h > p.content_h, "{q:?}");
        assert_sane(&q, 128);
    }

    #[test]
    fn inner_size_excludes_the_border() {
        let p = plan(12, DEFAULT_AREA);
        let (w, h) = p.inner_size();
        assert!((w - (p.cell_w - 2.0)).abs() < 0.001 && (h - (p.cell_h - 2.0)).abs() < 0.001);
    }

    #[test]
    fn degenerate_sizes_do_not_panic() {
        for (count, w, h) in [(0, 0.0, 0.0), (1, 10.0, 10.0), (5, 30.0, 1000.0), (200, 5000.0, 5.0), (3, 1000.0, 1000.0)] {
            let p = grid_plan(count, w, h);
            assert!(p.cols >= 1 && p.rows >= 1 && p.cell_w.is_finite() && p.cell_h.is_finite(), "{p:?}");
        }
    }
}
