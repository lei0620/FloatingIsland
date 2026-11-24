//! 光标资源与消息只属于岛体的输入窗口。

use std::cell::{Cell, OnceCell};

use eframe::egui::CursorIcon;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::GetCapture;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateCursor, DestroyCursor, GetCursorPos, HCURSOR, HTCLIENT, PostMessageW, SendMessageW,
    SetCursor, WindowFromPoint, WM_APP, WM_SETCURSOR,
};

const REFRESH_CURSOR: u32 = WM_APP + 0x151;
const SIZE: usize = 32;
const ROW_BYTES: usize = SIZE / 8;

// # 是黑色描边，. 是白色填充，其余像素透明。
const OPEN_HAND: &[&str] = &[
    "           ##",
    "          #..#",
    "       ## #..# ##",
    "      #..##..##..#",
    "      #..##..##..# ##",
    "      #..##..##..##..#",
    "      #..##..##..##..#",
    "      #..#............#",
    " ##   #...............#",
    "#..#  #...............#",
    "#...###...............#",
    " #....................#",
    "  #...................#",
    "   #.................#",
    "    #................#",
    "     #..............#",
    "      #.............#",
    "       #...........#",
    "        #..........#",
    "        #..........#",
    "        ############",
];

const CLOSED_HAND: &[&str] = &[
    "        ####",
    "      ##....###",
    "     #.........###",
    "     #............##",
    "     #..##..##..#...#",
    "     #.#..##..##..#.#",
    "  ###.#............##",
    " #...#..............#",
    " #..................#",
    "  #.................#",
    "   #...............#",
    "    #..............#",
    "     #............#",
    "      #...........#",
    "      #...........#",
    "      #############",
];

struct Hand(HCURSOR);

impl Hand {
    fn create(rows: &[&str], inset: (usize, usize)) -> Option<Self> {
        let mut and_plane = [0xffu8; SIZE * ROW_BYTES];
        let mut xor_plane = [0u8; SIZE * ROW_BYTES];
        for (row, pixels) in rows.iter().enumerate() {
            for (column, pixel) in pixels.bytes().enumerate() {
                if !matches!(pixel, b'#' | b'.') { continue; }
                let x = inset.0 + column;
                let y = inset.1 + row;
                let byte = y * ROW_BYTES + x / 8;
                let bit = 0x80 >> (x % 8);
                and_plane[byte] &= !bit;
                if pixel == b'.' { xor_plane[byte] |= bit; }
            }
        }
        unsafe {
            CreateCursor(None, 15, 14, SIZE as i32, SIZE as i32,
                and_plane.as_ptr().cast(), xor_plane.as_ptr().cast()).ok().map(Self)
        }
    }
}

impl Drop for Hand {
    fn drop(&mut self) {
        unsafe { let _ = DestroyCursor(self.0); }
    }
}

thread_local! {
    static OPEN_CURSOR: OnceCell<Option<Hand>> = const { OnceCell::new() };
    static CLOSED_CURSOR: OnceCell<Option<Hand>> = const { OnceCell::new() };
    static ACTIVE_CURSOR: Cell<Option<HCURSOR>> = const { Cell::new(None) };
}

pub(super) fn set(hwnd: HWND, icon: CursorIcon) -> bool {
    let cursor = match icon {
        CursorIcon::Grab => OPEN_CURSOR.with(|slot| {
            slot.get_or_init(|| Hand::create(OPEN_HAND, (3, 3))).as_ref().map(|hand| hand.0)
        }),
        CursorIcon::Grabbing => CLOSED_CURSOR.with(|slot| {
            slot.get_or_init(|| Hand::create(CLOSED_HAND, (4, 6))).as_ref().map(|hand| hand.0)
        }),
        _ => None,
    };
    if ACTIVE_CURSOR.with(|active| active.replace(cursor)) != cursor {
        // 等本帧的平台输出生效后再刷新，也能在鼠标静止时切换或恢复。
        unsafe { let _ = PostMessageW(hwnd, REFRESH_CURSOR, WPARAM(0), LPARAM(0)); }
    }
    cursor.is_some()
}

pub(super) unsafe fn window_message(hwnd: HWND, msg: u32, lp: LPARAM) -> Option<LRESULT> {
    if msg == REFRESH_CURSOR {
        let mut pointer = POINT::default();
        if GetCapture() == hwnd || GetCursorPos(&mut pointer).is_ok() && WindowFromPoint(pointer) == hwnd {
            SendMessageW(hwnd, WM_SETCURSOR, WPARAM(hwnd.0 as usize), LPARAM(HTCLIENT as isize));
        }
        return Some(LRESULT(0));
    }
    if msg == WM_SETCURSOR && lp.0 as u16 == HTCLIENT as u16 {
        if let Some(cursor) = ACTIVE_CURSOR.get() {
            SetCursor(cursor);
            return Some(LRESULT(1));
        }
    }
    None
}

#[cfg(test)]
#[test]
fn windows_drag_cursors_create_distinct_handles_with_matching_hotspots() {
    use windows::Win32::Graphics::Gdi::DeleteObject;
    use windows::Win32::UI::WindowsAndMessaging::{GetIconInfo, HICON, ICONINFO};

    let open = Hand::create(OPEN_HAND, (3, 3)).expect("Windows 应能创建张开手光标");
    let closed = Hand::create(CLOSED_HAND, (4, 6)).expect("Windows 应能创建握紧手光标");
    assert_ne!(open.0, closed.0);
    for cursor in [&open, &closed] {
        let mut info = ICONINFO::default();
        unsafe {
            GetIconInfo(HICON(cursor.0.0), &mut info).unwrap();
            let _ = DeleteObject(info.hbmMask);
            if !info.hbmColor.is_invalid() { let _ = DeleteObject(info.hbmColor); }
        }
        assert!(!info.fIcon.as_bool());
        assert_eq!((info.xHotspot, info.yHotspot), (15, 14));
    }
}
