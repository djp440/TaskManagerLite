//! 提取可执行文件的图标并转成 RGBA 像素，不依赖界面库。
//!
//! 调用较慢（要读文件资源），应放在后台线程。图标按请求的像素尺寸取，
//! 高 DPI 下请求物理像素尺寸即可保持清晰。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, GetObjectW,
    HBITMAP, ReleaseDC,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, GetIconInfo, HICON, ICONINFO, IDI_APPLICATION, IMAGE_ICON, LR_SHARED, LoadImageW, PrivateExtractIconsW,
};

/// 一个图标的像素，RGBA 字节序，非预乘，自上而下。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconBitmap {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl IconBitmap {
    /// 是否存在不透明度大于 0 的像素（全透明的图标没有意义）。
    pub fn has_visible_pixels(&self) -> bool {
        self.rgba.chunks_exact(4).any(|p| p[3] != 0)
    }
}

struct OwnedIcon(HICON);

impl Drop for OwnedIcon {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyIcon(self.0);
        }
    }
}

struct OwnedBitmap(HBITMAP);

impl Drop for OwnedBitmap {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            unsafe {
                let _ = DeleteObject(self.0.into());
            }
        }
    }
}

/// 读取 GDI 位图的 32 位 BGRA 像素（自上而下）。
fn read_bitmap(bitmap: HBITMAP) -> Option<(u32, u32, Vec<u8>)> {
    let mut bmp = BITMAP::default();
    let got = unsafe { GetObjectW(bitmap.into(), size_of::<BITMAP>() as i32, Some((&mut bmp as *mut BITMAP).cast())) };
    if got == 0 || bmp.bmWidth <= 0 || bmp.bmHeight <= 0 {
        return None;
    }
    let (w, h) = (bmp.bmWidth, bmp.bmHeight);
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut px = vec![0u8; w as usize * h as usize * 4];
    let hdc = unsafe { GetDC(None) };
    let lines = unsafe { GetDIBits(hdc, bitmap, 0, h as u32, Some(px.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS) };
    unsafe { ReleaseDC(Some(HWND::default()), hdc) };
    (lines == h).then_some((w as u32, h as u32, px))
}

/// HICON 转 RGBA。带 alpha 通道的图标直接用颜色位图；
/// 旧式图标（alpha 全为 0）用掩码位图补出透明区域。
fn hicon_to_rgba(icon: HICON) -> Option<IconBitmap> {
    let mut info = ICONINFO::default();
    unsafe { GetIconInfo(icon, &mut info) }.ok()?;
    let color = OwnedBitmap(info.hbmColor);
    let mask = OwnedBitmap(info.hbmMask);
    // 单色图标没有颜色位图，这里不支持。
    if color.0.is_invalid() {
        return None;
    }
    let (width, height, mut px) = read_bitmap(color.0)?;
    let has_alpha = px.chunks_exact(4).any(|p| p[3] != 0);
    if !has_alpha
        && let Some((mw, mh, m)) = read_bitmap(mask.0)
        && (mw, mh) == (width, height)
    {
        // 掩码：黑色（0）= 不透明，白色 = 透明。
        for (p, mp) in px.chunks_exact_mut(4).zip(m.chunks_exact(4)) {
            p[3] = if mp[0] == 0 { 255 } else { 0 };
        }
    } else if !has_alpha {
        // 没有可用的掩码时当作全部不透明。
        for p in px.chunks_exact_mut(4) {
            p[3] = 255;
        }
    }
    // BGRA -> RGBA
    for p in px.chunks_exact_mut(4) {
        p.swap(0, 2);
    }
    Some(IconBitmap { width, height, rgba: px })
}

fn to_wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().chain(std::iter::once(0)).collect()
}

/// 按目标像素尺寸直接从文件资源里取最接近的图标。`index` 为非负时是图标序号，
/// 为负时取资源 ID（`-id`）；文件没有图标资源时失败。只读文件资源，不加载外壳。
fn extract_indexed(path: &Path, index: i32, px: u32) -> Option<IconBitmap> {
    let wide = to_wide(path);
    // PrivateExtractIconsW 的文件名参数是固定的 MAX_PATH 缓冲区。
    if wide.len() > 260 {
        return None;
    }
    let mut name = [0u16; 260];
    name[..wide.len()].copy_from_slice(&wide);
    let mut icons = [HICON::default()];
    let n = unsafe { PrivateExtractIconsW(&name, index, px as i32, px as i32, Some(&mut icons), None, 0) };
    if n == 0 || n == u32::MAX || icons[0].is_invalid() {
        return None;
    }
    let owned = OwnedIcon(icons[0]);
    hicon_to_rgba(owned.0)
}

/// 缩放到恰好 `px` × `px`（最近邻，仅在系统给的尺寸与请求不一致时使用）。
fn fit_to(icon: IconBitmap, px: u32) -> IconBitmap {
    if icon.width == px && icon.height == px {
        return icon;
    }
    // 双线性缩放，避免尺寸差一两个像素时出现锯齿。
    let (sw, sh) = (icon.width as f32, icon.height as f32);
    let mut out = vec![0u8; px as usize * px as usize * 4];
    let sample = |x: i64, y: i64, c: usize| -> f32 {
        let x = x.clamp(0, icon.width as i64 - 1) as usize;
        let y = y.clamp(0, icon.height as i64 - 1) as usize;
        icon.rgba[(y * icon.width as usize + x) * 4 + c] as f32
    };
    for y in 0..px {
        for x in 0..px {
            let fx = (x as f32 + 0.5) * sw / px as f32 - 0.5;
            let fy = (y as f32 + 0.5) * sh / px as f32 - 0.5;
            let (x0, y0) = (fx.floor(), fy.floor());
            let (tx, ty) = (fx - x0, fy - y0);
            let (x0, y0) = (x0 as i64, y0 as i64);
            // 按 alpha 加权插值，避免透明边缘的颜色渗出。
            let mut acc = [0f32; 3];
            let mut alpha = 0f32;
            for (dx, dy, w) in [
                (0, 0, (1.0 - tx) * (1.0 - ty)),
                (1, 0, tx * (1.0 - ty)),
                (0, 1, (1.0 - tx) * ty),
                (1, 1, tx * ty),
            ] {
                let a = sample(x0 + dx, y0 + dy, 3);
                alpha += a * w;
                for (c, v) in acc.iter_mut().enumerate() {
                    *v += sample(x0 + dx, y0 + dy, c) * a * w;
                }
            }
            let o = (y as usize * px as usize + x as usize) * 4;
            if alpha > 0.0 {
                for c in 0..3 {
                    out[o + c] = (acc[c] / alpha).round().clamp(0.0, 255.0) as u8;
                }
            }
            out[o + 3] = alpha.round().clamp(0.0, 255.0) as u8;
        }
    }
    IconBitmap { width: px, height: px, rgba: out }
}

/// 系统共享图标（`IDI_APPLICATION`），按 `px` 取最接近的尺寸。
/// imageres.dll 的通用 exe 图标取不到时（精简环境）作为最后的兜底。
/// 共享图标由系统持有，不能 `DestroyIcon`，所以不包进 `OwnedIcon`。
pub(crate) fn system_app_icon(px: u32) -> Option<IconBitmap> {
    let handle = unsafe { LoadImageW(None, IDI_APPLICATION, IMAGE_ICON, px as i32, px as i32, LR_SHARED) }.ok()?;
    let icon = hicon_to_rgba(HICON(handle.0))?;
    icon.has_visible_pixels().then(|| fit_to(icon, px))
}

/// 取到且非全透明的图标缩放到 `px`，否则为 `None`。
fn usable(icon: Option<IconBitmap>, px: u32) -> Option<IconBitmap> {
    icon.filter(IconBitmap::has_visible_pixels).map(|i| fit_to(i, px))
}

/// 系统自带的“可执行文件通用图标”（蓝色应用窗口）：`imageres.dll` 的资源 ID 15，
/// 与外壳给没有图标的 exe 显示的是同一个。资源 ID 比序号稳定；取不到时由调用方继续回退。
const GENERIC_EXE_ICON_RESOURCE: i32 = -15;

pub(crate) fn generic_exe_icon(px: u32) -> Option<IconBitmap> {
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    usable(extract_indexed(&root.join("System32").join("imageres.dll"), GENERIC_EXE_ICON_RESOURCE, px), px)
}

/// 可执行文件的图标，边长恰为 `px` 像素。文件不存在时返回 `None`；
/// 文件存在但取不到图标（没有图标资源，如 svchost）时回退到 [`default_icon`]。
/// 全程只读文件资源，不使用外壳（`SHGetFileInfo` 会把整套外壳 DLL 拉进进程，约 6 MB，且并发调用会失败）。
pub fn file_icon(path: &Path, px: u32) -> Option<IconBitmap> {
    if !path.exists() {
        return None;
    }
    usable(extract_indexed(path, 0, px), px).or_else(|| default_icon(px))
}

/// 通用的可执行文件图标，用于拿不到路径（System、受保护进程）和没有图标资源的进程。
/// 依次尝试 imageres.dll 的通用 exe 图标与系统应用程序图标；每个尺寸只取一次并缓存。
pub fn default_icon(px: u32) -> Option<IconBitmap> {
    static CACHE: Mutex<Option<HashMap<u32, Option<IconBitmap>>>> = Mutex::new(None);
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .get_or_insert_with(HashMap::new)
        .entry(px)
        .or_insert_with(|| generic_exe_icon(px).or_else(|| system_app_icon(px)))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_app_icon_works_without_the_shell() {
        // 最后的兜底，不依赖外壳与任何文件资源。
        for px in [16, 32] {
            let icon = system_app_icon(px).expect("系统应用程序图标");
            assert_eq!((icon.width, icon.height), (px, px));
            assert!(icon.has_visible_pixels());
        }
    }

    #[test]
    fn generic_exe_icon_comes_from_imageres_without_the_shell() {
        for px in [16, 32, 48] {
            let icon = generic_exe_icon(px).expect("imageres.dll 的通用 exe 图标");
            assert_eq!((icon.width, icon.height), (px, px));
            assert!(icon.has_visible_pixels());
        }
        // 与系统应用程序图标（兜底）不是同一张。
        assert_ne!(generic_exe_icon(32), system_app_icon(32));
    }

    #[test]
    fn default_icon_is_cached_per_size() {
        let a = default_icon(24).expect("默认图标");
        assert_eq!((a.width, a.height), (24, 24));
        assert_eq!(default_icon(24), Some(a));
        let b = default_icon(40).expect("默认图标");
        assert_eq!((b.width, b.height), (40, 40));
    }

    #[test]
    fn exe_without_icon_resource_falls_back_to_default_icon() {
        // 测试可执行文件本身没有图标资源。
        let me = std::env::current_exe().unwrap();
        assert!(extract_indexed(&me, 0, 32).is_none());
        assert_eq!(file_icon(&me, 32), default_icon(32));
    }

    #[test]
    fn fit_keeps_exact_size_and_resizes_others() {
        let src = IconBitmap { width: 2, height: 2, rgba: [[255u8, 0, 0, 255]; 4].into_iter().flatten().collect() };
        let same = fit_to(src.clone(), 2);
        assert_eq!(same, src);
        let big = fit_to(src, 8);
        assert_eq!((big.width, big.height), (8, 8));
        assert!(big.rgba.chunks_exact(4).all(|p| p == [255, 0, 0, 255]));
    }

    #[test]
    fn fit_does_not_bleed_color_from_transparent_pixels() {
        // 左半透明（颜色为绿），右半不透明红：缩放后不应出现带绿色的半透明红边。
        let mut rgba = Vec::new();
        for _ in 0..2 {
            rgba.extend_from_slice(&[0, 255, 0, 0]);
            rgba.extend_from_slice(&[255, 0, 0, 255]);
        }
        let out = fit_to(IconBitmap { width: 2, height: 2, rgba }, 6);
        for p in out.rgba.chunks_exact(4).filter(|p| p[3] > 0) {
            assert_eq!((p[0], p[1], p[2]), (255, 0, 0));
        }
    }
}
