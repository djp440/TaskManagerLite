//! 提取可执行文件的图标并转成 RGBA 像素，不依赖界面库。
//!
//! 调用较慢（要读文件资源），应放在后台线程。图标按请求的像素尺寸取，
//! 高 DPI 下请求物理像素尺寸即可保持清晰。

use std::path::Path;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::{
    BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, GetObjectW,
    HBITMAP, ReleaseDC,
};
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::UI::Shell::{SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_SMALLICON, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW};
use windows::Win32::UI::WindowsAndMessaging::{DestroyIcon, GetIconInfo, HICON, ICONINFO, PrivateExtractIconsW};
use windows::core::PCWSTR;

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

/// 该线程第一次调用时初始化 COM（外壳 API 需要）；线程退出时由系统回收。
pub(crate) fn ensure_com() {
    thread_local! {
        static INIT: () = unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        };
    }
    INIT.with(|_| {});
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

/// 按目标像素尺寸直接从文件资源里取最接近的图标；文件没有图标资源时失败。
fn extract_sized(path: &Path, px: u32) -> Option<IconBitmap> {
    let wide = to_wide(path);
    // PrivateExtractIconsW 的文件名参数是固定的 MAX_PATH 缓冲区。
    if wide.len() > 260 {
        return None;
    }
    let mut name = [0u16; 260];
    name[..wide.len()].copy_from_slice(&wide);
    let mut icons = [HICON::default()];
    let n = unsafe { PrivateExtractIconsW(&name, 0, px as i32, px as i32, Some(&mut icons), None, 0) };
    if n == 0 || n == u32::MAX || icons[0].is_invalid() {
        return None;
    }
    let owned = OwnedIcon(icons[0]);
    hicon_to_rgba(owned.0)
}

/// 用外壳取图标（含关联图标与通用图标）。`path` 为不存在的文件时配合 `use_attributes` 取按扩展名的通用图标。
fn shell_icon(path: &Path, px: u32, use_attributes: bool) -> Option<IconBitmap> {
    ensure_com();
    let wide = to_wide(path);
    let mut info = SHFILEINFOW::default();
    let mut flags = SHGFI_ICON | if px > 20 { SHGFI_LARGEICON } else { SHGFI_SMALLICON };
    if use_attributes {
        flags |= SHGFI_USEFILEATTRIBUTES;
    }
    let ok = unsafe {
        SHGetFileInfoW(
            PCWSTR(wide.as_ptr()),
            FILE_ATTRIBUTE_NORMAL,
            Some(&mut info),
            size_of::<SHFILEINFOW>() as u32,
            flags,
        )
    };
    if ok == 0 || info.hIcon.is_invalid() {
        return None;
    }
    let owned = OwnedIcon(info.hIcon);
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

/// 可执行文件的图标，边长恰为 `px` 像素。文件不存在或取不到时返回 `None`。
pub fn file_icon(path: &Path, px: u32) -> Option<IconBitmap> {
    let icon = extract_sized(path, px).or_else(|| shell_icon(path, px, false))?;
    icon.has_visible_pixels().then(|| fit_to(icon, px))
}

/// 通用的可执行文件图标，用于拿不到路径（System、受保护进程）的进程。
pub fn default_icon(px: u32) -> Option<IconBitmap> {
    // 文件不必存在：只按扩展名取通用图标。
    let icon = shell_icon(Path::new("x.exe"), px, true)?;
    icon.has_visible_pixels().then(|| fit_to(icon, px))
}

#[cfg(test)]
mod tests {
    use super::*;

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
