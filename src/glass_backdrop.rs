//! Windows 桌面合成背景。只读回岛体附近的像素，模糊后留在显存中。

use std::{mem::size_of, time::{Duration, Instant}};

use eframe::egui::{self, Pos2, Rect};
use windows::{
    core::{w, Interface},
    Win32::{
        Foundation::{BOOL, HMODULE, HWND, RECT},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
            Direct3D11::*,
            Dxgi::{Common::*, *},
            Gdi::{MonitorFromWindow, HMONITOR, MONITOR_DEFAULTTONEAREST},
        },
        System::LibraryLoader::{GetModuleHandleW, GetProcAddress},
        UI::WindowsAndMessaging::GetWindowRect,
    },
};

const REFRESH: Duration = Duration::from_millis(100);
const RETRY: Duration = Duration::from_secs(2);

// 排除本窗口以防止反馈循环。该属性只作用于桌面复制 API。
fn exclude_from_duplication(window: HWND, exclude: bool) -> bool {
    #[repr(C)]
    struct WindowAttribute {
        attribute: i32,
        value: *const BOOL,
        size: usize,
    }
    type SetWindowAttribute = unsafe extern "system" fn(HWND, *const WindowAttribute) -> BOOL;
    unsafe {
        let Ok(module) = GetModuleHandleW(w!("user32.dll")) else { return false; };
        let Some(proc) = GetProcAddress(module, windows::core::s!("SetWindowCompositionAttribute")) else { return false; };
        let set: SetWindowAttribute = std::mem::transmute(proc);
        let value = BOOL::from(exclude);
        let attribute = WindowAttribute { attribute: 24, value: &value, size: size_of::<BOOL>() };
        set(window, &attribute).as_bool()
    }
}

struct Desktop {
    monitor: HMONITOR,
    bounds: RECT,
    duplication: IDXGIOutputDuplication,
    device: ID3D11Device,
    context: ID3D11DeviceContext,
    staging: Option<(u32, u32, ID3D11Texture2D)>,
}

impl Desktop {
    fn open(monitor: HMONITOR) -> Option<Self> {
        unsafe {
            let factory = CreateDXGIFactory1::<IDXGIFactory1>().ok()?;
            for adapter in (0..).map_while(|index| factory.EnumAdapters1(index).ok()) {
                for output in (0..).map_while(|index| adapter.EnumOutputs(index).ok()) {
                    let desc = output.GetDesc().ok()?;
                    if desc.Monitor != monitor { continue; }
                    if desc.Rotation != DXGI_MODE_ROTATION_IDENTITY { return None; }
                    let (mut device, mut context) = (None, None);
                    D3D11CreateDevice(&adapter, D3D_DRIVER_TYPE_UNKNOWN, HMODULE::default(),
                        D3D11_CREATE_DEVICE_FLAG(0), None, D3D11_SDK_VERSION,
                        Some(&mut device), None, Some(&mut context)).ok()?;
                    let (device, context) = (device?, context?);
                    let duplication = output.cast::<IDXGIOutput1>().ok()?.DuplicateOutput(&device).ok()?;
                    return Some(Self { monitor, bounds: desc.DesktopCoordinates, duplication, device, context, staging: None });
                }
            }
        }
        None
    }

    fn image(&mut self, region: RECT, step: u32, sigma: f32) -> windows::core::Result<Option<egui::ColorImage>> {
        let width = (region.right - region.left) as u32;
        let height = (region.bottom - region.top) as u32;
        unsafe {
            if self.staging.as_ref().is_none_or(|(w, h, _)| (*w, *h) != (width, height)) {
                let mut staging = None;
                self.device.CreateTexture2D(&D3D11_TEXTURE2D_DESC {
                    Width: width, Height: height, MipLevels: 1, ArraySize: 1,
                    Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_STAGING, CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    ..Default::default()
                }, None, Some(&mut staging))?;
                self.staging = staging.map(|texture| (width, height, texture));
            }
            let Some((_, _, staging)) = &self.staging else { return Ok(None); };
            let mut resource = None;
            match self.duplication.AcquireNextFrame(0, &mut DXGI_OUTDUPL_FRAME_INFO::default(), &mut resource) {
                Ok(()) => {}
                Err(error) if error.code() == DXGI_ERROR_WAIT_TIMEOUT => return Ok(None),
                Err(error) => return Err(error),
            }
            let box_ = D3D11_BOX {
                left: (region.left - self.bounds.left) as u32, right: (region.right - self.bounds.left) as u32,
                top: (region.top - self.bounds.top) as u32, bottom: (region.bottom - self.bounds.top) as u32,
                front: 0, back: 1,
            };
            let source = resource.and_then(|resource| resource.cast::<ID3D11Texture2D>().ok());
            if let Some(source) = &source {
                self.context.CopySubresourceRegion(staging, 0, 0, 0, 0, source, 0, Some(&box_));
            }
            self.duplication.ReleaseFrame()?;
            if source.is_none() { return Ok(None); }
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            self.context.Map(staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let sw = width.div_ceil(step) as usize;
            let sh = height.div_ceil(step) as usize;
            let mut rgb = Vec::with_capacity(sw * sh);
            for y in 0..sh as u32 {
                let row = mapped.pData.cast::<u8>().add((y * step + step / 2).min(height - 1) as usize * mapped.RowPitch as usize);
                for x in 0..sw as u32 {
                    let bgr = row.add((x * step + step / 2).min(width - 1) as usize * 4);
                    rgb.push([*bgr.add(2), *bgr.add(1), *bgr]);
                }
            }
            self.context.Unmap(staging, 0);
            Ok(Some(crate::glass::blur_backdrop([sw, sh], &rgb, sigma)))
        }
    }
}

#[derive(Default)]
pub struct Backdrop {
    desktop: Option<Desktop>,
    excluded: bool,
    next: Option<Instant>,
    region: Option<RECT>,
}

impl Backdrop {
    pub fn update(&mut self, ctx: &egui::Context, hwnd: HWND, active: bool) {
        if !active {
            if self.excluded {
                exclude_from_duplication(hwnd, false);
                *self = Self::default();
                crate::glass::set_backdrop(ctx, None);
            }
            return;
        }
        let now = Instant::now();
        if let Some(next) = self.next.filter(|next| *next > now) {
            ctx.request_repaint_after(next - now);
            return;
        }
        let wait = self.refresh(ctx, hwnd).map_or(RETRY, |_| REFRESH);
        self.next = Some(now + wait);
        ctx.request_repaint_after(wait);
    }

    fn refresh(&mut self, ctx: &egui::Context, hwnd: HWND) -> Option<()> {
        self.excluded = self.excluded || exclude_from_duplication(hwnd, true);
        if !self.excluded { return None; }
        let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
        if self.desktop.as_ref().is_none_or(|desktop| desktop.monitor != monitor) {
            self.desktop = Desktop::open(monitor);
            self.region = None;
            crate::glass::set_backdrop(ctx, None);
        }
        let desktop = self.desktop.as_mut()?;
        let mut window = RECT::default();
        unsafe { GetWindowRect(hwnd, &mut window).ok()?; }
        let region = RECT {
            left: window.left.max(desktop.bounds.left),
            top: window.top.max(desktop.bounds.top),
            right: window.right.min(desktop.bounds.right),
            bottom: window.bottom.min(desktop.bounds.bottom),
        };
        if region.right <= region.left || region.bottom <= region.top { return None; }
        if self.region != Some(region) {
            self.region = Some(region);
            crate::glass::set_backdrop(ctx, None);
        }
        let scale = ctx.pixels_per_point();
        let step = (4.0 * scale).round().max(1.0) as u32;
        match desktop.image(region, step, crate::glass::BACKDROP_SIGMA * scale / step as f32) {
            Ok(Some(image)) => {
                let point = |x, y| Pos2::new((x - window.left) as f32 / scale, (y - window.top) as f32 / scale);
                crate::glass::set_backdrop(ctx, Some((Rect::from_min_max(
                    point(region.left, region.top), point(region.right, region.bottom)), image)));
            }
            Ok(None) => {}
            Err(_) => {
                self.desktop = None;
                crate::glass::set_backdrop(ctx, None);
                return None;
            }
        }
        Some(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::{
        Graphics::{Dwm::DwmFlush, Gdi::UpdateWindow},
        UI::WindowsAndMessaging::*,
    };

    #[test]
    #[ignore = "使用交互式 Windows 桌面和合成测试窗口验证原生背景"]
    fn capture_blurs_the_scene_behind_the_excluded_window() {
        struct TestWindow(HWND);
        impl Drop for TestWindow {
            fn drop(&mut self) { unsafe { let _ = DestroyWindow(self.0); } }
        }
        let window = |x, width, style| {
            TestWindow(unsafe {
                CreateWindowExW(WS_EX_TOPMOST | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                    w!("STATIC"), w!(""), WS_POPUP | WS_VISIBLE | WINDOW_STYLE(style),
                    x, 190, width, 120, None, None, None, None).unwrap()
            })
        };
        let left = window(300, 120, 4); // 黑色测试色块
        let right = window(420, 120, 6); // 白色测试色块
        let overlay = window(330, 180, 5);
        assert!(exclude_from_duplication(overlay.0, true));
        unsafe {
            for hwnd in [left.0, right.0, overlay.0] { let _ = UpdateWindow(hwnd); }
            DwmFlush().unwrap();
        }
        let monitor = unsafe { MonitorFromWindow(overlay.0, MONITOR_DEFAULTTONEAREST) };
        let mut desktop = Desktop::open(monitor).expect("可复制测试窗口所在的桌面");
        let region = RECT { left: 340, right: 500, top: 220, bottom: 280 };
        let image = (0..30).find_map(|_| {
            std::thread::sleep(Duration::from_millis(25));
            desktop.image(region, 4, crate::glass::BACKDROP_SIGMA / 4.0).unwrap()
        }).expect("获得桌面帧");
        let row = &image.pixels[7 * 40..8 * 40];
        eprintln!("背后色块与模糊交界：{} / {} / {}", row[5].r(), row[19].r(), row[35].r());
        assert!(row[5].r() < row[35].r(), "排除测试窗后能看到下方两种底色");
        assert!(row[35].r() - row[5].r() >= 40, "模糊后应保留色块明暗差异");
        assert!(row[19].r() < row[35].r() && row[19].r().abs_diff(row[20].r()) < 20,
            "色块交界应经过高斯卷积");
    }
}
