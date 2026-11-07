use std::ffi::OsStr;
use std::mem::{size_of, transmute, zeroed};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::process::CommandExt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr::{copy_nonoverlapping, null, null_mut};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Storage::EnhancedStorage::PKEY_Home_IsPinned;
use windows::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::System::SystemServices::SFGAO_FOLDER;
use windows::Win32::UI::Shell::{
    BHID_EnumItems, IEnumShellItems, IShellItem, IShellItem2, SHCreateItemFromParsingName, SIGDN, SIGDN_FILESYSPATH,
    SIGDN_NORMALDISPLAY,
};
use windows::core::{HSTRING, Interface};
use windows_sys::Win32::Foundation::{FILETIME, GlobalFree, HWND, MAX_PATH, SYSTEMTIME};
use windows_sys::Win32::Graphics::Dwm::{
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmExtendFrameIntoClientArea, DwmSetWindowAttribute,
};
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, MoveFileW,
};
use windows_sys::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, GetClipboardSequenceNumber, IsClipboardFormatAvailable,
    OpenClipboard, RegisterClipboardFormatW, SetClipboardData,
};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows_sys::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};
use windows_sys::Win32::System::Time::{
    FileTimeToSystemTime, SystemTimeToFileTime, SystemTimeToTzSpecificLocalTime, TzSpecificLocalTimeToSystemTime,
};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;
use windows_sys::Win32::UI::Controls::MARGINS;
use windows_sys::Win32::UI::Controls::Dialogs::{
    GetSaveFileNameW, OFN_HIDEREADONLY, OFN_OVERWRITEPROMPT, OPENFILENAMEW,
};
use windows_sys::Win32::UI::Shell::{
    DROPFILES, DragQueryFileW, FO_COPY, FO_DELETE, FO_MOVE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_RENAMEONCOLLISION,
    FOF_WANTNUKEWARNING, HDROP, SEE_MASK_INVOKEIDLIST, SHELLEXECUTEINFOW, SHEmptyRecycleBinW, SHFILEOPSTRUCTW, SHFileOperationW, SHQUERYRBINFO, SHQueryRecycleBinW,
    ShellExecuteExW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    IDYES, MB_ICONERROR, MB_ICONWARNING, MB_OK, MB_TASKMODAL, MB_YESNO, MessageBoxW, SW_SHOWNORMAL,
};

pub const QUICK_ACCESS: &str = "shell:::{679f85cb-0220-4080-b29b-5540cc05aab6}";

const UNIX_EPOCH_TICKS: u64 = 116_444_736_000_000_000;
const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
const SET_PREFERRED_APP_MODE_ORDINAL: usize = 135;
const FLUSH_MENU_THEMES_ORDINAL: usize = 136;
const APP_MODE_ALLOW_DARK: i32 = 1;
const DROP_EFFECT_COPY: u32 = 1;
const DROP_EFFECT_MOVE: u32 = 2;
const ALL_DROPPED_FILES: u32 = u32::MAX;

fn wide(text: impl AsRef<OsStr>) -> Vec<u16> {
    text.as_ref().encode_wide().chain([0]).collect()
}

fn to_filetime(ticks: u64) -> FILETIME {
    FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 }
}

pub fn now_filetime() -> u64 {
    let since_epoch = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    UNIX_EPOCH_TICKS + (since_epoch.as_nanos() / 100) as u64
}

pub fn format_time(ticks: u64) -> String {
    if ticks == 0 {
        return String::new();
    }
    let mut utc: SYSTEMTIME = unsafe { zeroed() };
    let mut local: SYSTEMTIME = unsafe { zeroed() };
    let converted = unsafe {
        FileTimeToSystemTime(&to_filetime(ticks), &mut utc) != 0
            && SystemTimeToTzSpecificLocalTime(null(), &utc, &mut local) != 0
    };
    if !converted {
        return String::new();
    }
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}",
        local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute
    )
}

pub fn local_day_start(text: &str) -> Option<u64> {
    let mut parts = text.trim().splitn(3, '-').map(|part| part.parse::<u16>().ok());
    let (year, month, day) = (parts.next()??, parts.next()??, parts.next()??);
    let local = SYSTEMTIME { wYear: year, wMonth: month, wDay: day, ..unsafe { zeroed() } };
    let mut utc: SYSTEMTIME = unsafe { zeroed() };
    let mut filetime: FILETIME = unsafe { zeroed() };
    let converted = unsafe {
        TzSpecificLocalTimeToSystemTime(null(), &local, &mut utc) != 0
            && SystemTimeToFileTime(&utc, &mut filetime) != 0
    };
    converted.then_some((filetime.dwHighDateTime as u64) << 32 | filetime.dwLowDateTime as u64)
}

pub fn allow_dark_menus() {
    unsafe {
        let uxtheme = LoadLibraryW(wide("uxtheme.dll").as_ptr());
        if uxtheme.is_null() {
            return;
        }
        if let Some(set_preferred_app_mode) = GetProcAddress(uxtheme, SET_PREFERRED_APP_MODE_ORDINAL as *const u8) {
            let set_preferred_app_mode: unsafe extern "system" fn(i32) -> i32 = transmute(set_preferred_app_mode);
            set_preferred_app_mode(APP_MODE_ALLOW_DARK);
        }
        if let Some(flush_menu_themes) = GetProcAddress(uxtheme, FLUSH_MENU_THEMES_ORDINAL as *const u8) {
            let flush_menu_themes: unsafe extern "system" fn() = transmute(flush_menu_themes);
            flush_menu_themes();
        }
    }
}

pub fn restore_frame_appearance(window: &slint::Window) {
    let window = window.window_handle();
    let Ok(handle) = window.window_handle() else { return };
    let RawWindowHandle::Win32(handle) = handle.as_raw() else { return };
    let hwnd = handle.hwnd.get() as HWND;
    let frame = MARGINS { cxLeftWidth: 1, cxRightWidth: 1, cyTopHeight: 1, cyBottomHeight: 1 };
    let corners = DWMWCP_ROUND;
    unsafe {
        DwmExtendFrameIntoClientArea(hwnd, &frame);
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&raw const corners).cast(),
            size_of_val(&corners) as u32,
        );
    }
}

#[derive(Clone, Debug)]
pub struct Drive {
    pub root: PathBuf,
    pub title: String,
    pub usage: Option<DiskUsage>,
}

#[derive(Clone, Copy, Debug)]
pub struct DiskUsage {
    pub total: u64,
    pub free: u64,
}

impl DiskUsage {
    pub fn used_ratio(self) -> f32 {
        if self.total == 0 { 0.0 } else { (self.total - self.free) as f32 / self.total as f32 }
    }
}

pub fn disk_usage(root: &Path) -> Option<DiskUsage> {
    let path = wide(root);
    let (mut total, mut free) = (0, 0);
    let found = unsafe { GetDiskFreeSpaceExW(path.as_ptr(), null_mut(), &mut total, &mut free) };
    (found != 0).then_some(DiskUsage { total, free })
}

fn volume_label(root: &[u16]) -> Option<String> {
    let mut label = [0u16; MAX_PATH as usize + 1];
    let found = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            null_mut(),
            null_mut(),
            null_mut(),
            null_mut(),
            0,
        )
    };
    let length = label.iter().position(|unit| *unit == 0)?;
    (found != 0 && length > 0).then(|| String::from_utf16_lossy(&label[..length]))
}

fn describe_drive(letter: char) -> Drive {
    let root = PathBuf::from(format!("{letter}:\\"));
    let path = wide(&root);
    let fallback = match unsafe { GetDriveTypeW(path.as_ptr()) } {
        DRIVE_REMOVABLE => "Removable Disk",
        DRIVE_REMOTE => "Network Drive",
        DRIVE_CDROM => "Optical Drive",
        _ => "Local Disk",
    };
    let label = volume_label(&path).unwrap_or_else(|| fallback.into());
    Drive { title: format!("{label} ({letter}:)"), usage: disk_usage(&root), root }
}

pub fn drives() -> Vec<Drive> {
    let mask = unsafe { GetLogicalDrives() };
    let letters: Vec<char> = ('A'..='Z').enumerate().filter(|(bit, _)| mask >> bit & 1 == 1).map(|(_, letter)| letter).collect();
    thread::scope(|scope| {
        let queries: Vec<_> = letters.iter().map(|letter| scope.spawn(|| describe_drive(*letter))).collect();
        queries.into_iter().filter_map(|query| query.join().ok()).collect()
    })
}

unsafe fn shell_name(item: &IShellItem, kind: SIGDN) -> Option<String> {
    unsafe {
        let name = item.GetDisplayName(kind).ok()?;
        let text = name.to_string().ok();
        CoTaskMemFree(Some(name.0.cast()));
        text
    }
}

unsafe fn pinned_folder(item: &IShellItem) -> Option<(PathBuf, String)> {
    unsafe {
        let properties: IShellItem2 = item.cast().ok()?;
        let pinned = properties.GetBool(&PKEY_Home_IsPinned).ok()?.as_bool();
        if !pinned || item.GetAttributes(SFGAO_FOLDER).ok()? != SFGAO_FOLDER {
            return None;
        }
        Some((PathBuf::from(shell_name(item, SIGDN_FILESYSPATH)?), shell_name(item, SIGDN_NORMALDISPLAY)?))
    }
}

unsafe fn enumerate_pins() -> windows::core::Result<Vec<(PathBuf, String)>> {
    unsafe {
        let quick_access: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(QUICK_ACCESS), None)?;
        let items: IEnumShellItems = quick_access.BindToHandler(None, &BHID_EnumItems)?;
        let next = || {
            let mut item = [None];
            items.Next(&mut item, None).ok()?;
            item[0].take()
        };
        Ok(std::iter::from_fn(next).filter_map(|item| pinned_folder(&item)).collect())
    }
}

pub fn quick_access_pins() -> Vec<(PathBuf, String)> {
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).is_ok();
        let pins = enumerate_pins().unwrap_or_default();
        if initialized {
            CoUninitialize();
        }
        pins
    }
}

fn shell_execute(verb: &str, path: &Path, mask: u32) -> bool {
    let verb = wide(verb);
    let file = wide(path);
    let mut info: SHELLEXECUTEINFOW = unsafe { zeroed() };
    info.cbSize = size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = mask;
    info.hwnd = unsafe { GetActiveWindow() };
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.nShow = SW_SHOWNORMAL;
    unsafe { ShellExecuteExW(&mut info) != 0 }
}

pub fn open(path: &Path) -> bool {
    shell_execute("open", path, 0)
}

pub fn show_properties(path: &Path) -> bool {
    shell_execute("properties", path, SEE_MASK_INVOKEIDLIST)
}

pub fn reveal_in_explorer(path: &Path) {
    let argument = format!("/select,\"{}\"", path.display());
    let _ = Command::new("explorer.exe").raw_arg(argument).spawn();
}

pub fn active_window() -> usize {
    unsafe { GetActiveWindow() as usize }
}

fn shell_operation(function: u32, paths: &[PathBuf], target: Option<&Path>, flags: u32, owner: usize) -> bool {
    let mut from: Vec<u16> = paths.iter().flat_map(wide).collect();
    from.push(0);
    let mut to: Vec<u16> = target.map(wide).unwrap_or_default();
    to.push(0);
    let mut operation: SHFILEOPSTRUCTW = unsafe { zeroed() };
    operation.hwnd = owner as _;
    operation.wFunc = function;
    operation.pFrom = from.as_ptr();
    operation.pTo = to.as_ptr();
    operation.fFlags = flags as u16;
    let failed = unsafe { SHFileOperationW(&mut operation) };
    failed == 0 && operation.fAnyOperationsAborted == 0
}

pub fn recycle(paths: &[PathBuf], owner: usize) -> bool {
    shell_operation(FO_DELETE, paths, None, FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING, owner)
}

pub fn copy_into(paths: &[PathBuf], target: &Path, owner: usize) -> bool {
    let same_folder = paths.iter().any(|path| path.parent() == Some(target));
    let collision = if same_folder { FOF_RENAMEONCOLLISION } else { 0 };
    shell_operation(FO_COPY, paths, Some(target), FOF_ALLOWUNDO | collision, owner)
}

pub fn move_into(paths: &[PathBuf], target: &Path, owner: usize) -> bool {
    shell_operation(FO_MOVE, paths, Some(target), FOF_ALLOWUNDO, owner)
}

pub fn rename(from: &Path, to: &Path) -> io::Result<()> {
    let (from, to) = (wide(from), wide(to));
    match unsafe { MoveFileW(from.as_ptr(), to.as_ptr()) } {
        0 => Err(io::Error::last_os_error()),
        _ => Ok(()),
    }
}

pub fn recycle_bin_size() -> Option<u64> {
    let mut info: SHQUERYRBINFO = unsafe { zeroed() };
    info.cbSize = size_of::<SHQUERYRBINFO>() as u32;
    let result = unsafe { SHQueryRecycleBinW(null(), &mut info) };
    (result == 0).then_some(info.i64Size as u64)
}

pub fn empty_recycle_bin() {
    unsafe { SHEmptyRecycleBinW(null_mut(), null(), 0) };
}

unsafe fn store_clipboard_entry(format: u32, bytes: &[u8]) -> bool {
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
        if memory.is_null() {
            return false;
        }
        let target = GlobalLock(memory).cast::<u8>();
        if target.is_null() {
            GlobalFree(memory);
            return false;
        }
        copy_nonoverlapping(bytes.as_ptr(), target, bytes.len());
        GlobalUnlock(memory);
        let stored = !SetClipboardData(format, memory).is_null();
        if !stored {
            GlobalFree(memory);
        }
        stored
    }
}

fn set_clipboard(entries: &[(u32, &[u8])]) -> bool {
    unsafe {
        if OpenClipboard(null_mut()) == 0 {
            return false;
        }
        EmptyClipboard();
        let stored = entries.iter().all(|(format, bytes)| store_clipboard_entry(*format, bytes));
        CloseClipboard();
        stored
    }
}

fn wide_bytes(units: &[u16]) -> impl Iterator<Item = u8> {
    units.iter().flat_map(|unit| unit.to_le_bytes())
}

pub fn copy_text(text: &str) -> bool {
    let bytes: Vec<u8> = wide_bytes(&wide(text)).collect();
    set_clipboard(&[(CF_UNICODETEXT as u32, &bytes)])
}

fn drop_effect_format() -> u32 {
    unsafe { RegisterClipboardFormatW(wide("Preferred DropEffect").as_ptr()) }
}

fn put_files_on_clipboard(paths: &[PathBuf], drop_effect: u32) -> bool {
    let mut names: Vec<u16> = paths.iter().flat_map(wide).collect();
    names.push(0);
    let mut header: DROPFILES = unsafe { zeroed() };
    header.pFiles = size_of::<DROPFILES>() as u32;
    header.fWide = 1;
    let header_bytes = unsafe {
        std::slice::from_raw_parts((&raw const header).cast::<u8>(), size_of::<DROPFILES>())
    };
    let bytes: Vec<u8> = header_bytes.iter().copied().chain(wide_bytes(&names)).collect();
    set_clipboard(&[(CF_HDROP as u32, &bytes), (drop_effect_format(), &drop_effect.to_le_bytes())])
}

pub fn copy_files(paths: &[PathBuf]) -> bool {
    put_files_on_clipboard(paths, DROP_EFFECT_COPY)
}

pub fn cut_files(paths: &[PathBuf]) -> bool {
    put_files_on_clipboard(paths, DROP_EFFECT_MOVE)
}

pub fn clipboard_has_files() -> bool {
    unsafe { IsClipboardFormatAvailable(CF_HDROP as u32) != 0 }
}

pub fn clipboard_sequence() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

pub fn clear_clipboard() {
    unsafe {
        if OpenClipboard(null_mut()) != 0 {
            EmptyClipboard();
            CloseClipboard();
        }
    }
}

pub struct ClipboardFiles {
    pub paths: Vec<PathBuf>,
    pub is_cut: bool,
}

unsafe fn dropped_paths(drop: HDROP) -> Vec<PathBuf> {
    unsafe {
        let count = DragQueryFileW(drop, ALL_DROPPED_FILES, null_mut(), 0);
        (0..count)
            .map(|index| {
                let length = DragQueryFileW(drop, index, null_mut(), 0) as usize;
                let mut name = vec![0u16; length + 1];
                DragQueryFileW(drop, index, name.as_mut_ptr(), name.len() as u32);
                name.truncate(length);
                PathBuf::from(std::ffi::OsString::from_wide(&name))
            })
            .collect()
    }
}

unsafe fn clipboard_drop_effect() -> u32 {
    unsafe {
        let memory = GetClipboardData(drop_effect_format());
        if memory.is_null() {
            return DROP_EFFECT_COPY;
        }
        let value = GlobalLock(memory).cast::<u32>();
        if value.is_null() {
            return DROP_EFFECT_COPY;
        }
        let effect = value.read_unaligned();
        GlobalUnlock(memory);
        effect
    }
}

pub fn clipboard_files() -> Option<ClipboardFiles> {
    unsafe {
        if OpenClipboard(null_mut()) == 0 {
            return None;
        }
        let drop = GetClipboardData(CF_HDROP as u32);
        let files = (!drop.is_null()).then(|| ClipboardFiles {
            paths: dropped_paths(drop),
            is_cut: clipboard_drop_effect() & DROP_EFFECT_MOVE != 0,
        });
        CloseClipboard();
        files
    }
}

pub fn confirm(title: &str, message: &str) -> bool {
    let (title, message) = (wide(title), wide(message));
    let style = MB_YESNO | MB_ICONWARNING | MB_TASKMODAL;
    unsafe { MessageBoxW(GetActiveWindow(), message.as_ptr(), title.as_ptr(), style) == IDYES }
}

pub fn alert(title: &str, message: &str) {
    let (title, message) = (wide(title), wide(message));
    let style = MB_OK | MB_ICONERROR | MB_TASKMODAL;
    unsafe { MessageBoxW(GetActiveWindow(), message.as_ptr(), title.as_ptr(), style) };
}

pub fn save_dialog(title: &str, file_name: &str, filters: &[(&str, &str)]) -> Option<(PathBuf, usize)> {
    let filter_text: String = filters
        .iter()
        .map(|(label, extension)| format!("{label}\0*.{extension}\0"))
        .collect();
    let filter = wide(filter_text);
    let title = wide(title);
    let extension = wide(filters.first().map_or("", |(_, extension)| extension));
    let mut path = [0u16; 32_768];
    let name: Vec<u16> = file_name.encode_utf16().take(path.len() - 1).collect();
    path[..name.len()].copy_from_slice(&name);
    let mut dialog: OPENFILENAMEW = unsafe { zeroed() };
    dialog.lStructSize = size_of::<OPENFILENAMEW>() as u32;
    dialog.hwndOwner = unsafe { GetActiveWindow() };
    dialog.lpstrFilter = filter.as_ptr();
    dialog.nFilterIndex = 1;
    dialog.lpstrFile = path.as_mut_ptr();
    dialog.nMaxFile = path.len() as u32;
    dialog.lpstrTitle = title.as_ptr();
    dialog.lpstrDefExt = extension.as_ptr();
    dialog.Flags = OFN_OVERWRITEPROMPT | OFN_HIDEREADONLY;
    if unsafe { GetSaveFileNameW(&mut dialog) } == 0 {
        return None;
    }
    let length = path.iter().position(|unit| *unit == 0)?;
    let chosen = PathBuf::from(std::ffi::OsString::from_wide(&path[..length]));
    Some((chosen, dialog.nFilterIndex.saturating_sub(1) as usize))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_round_trip_through_a_local_day() {
        let start = local_day_start("2024-03-15").unwrap();
        assert_eq!(format_time(start), "2024-03-15 00:00");
    }

    #[test]
    fn impossible_dates_are_rejected() {
        assert!(local_day_start("2024-13-40").is_none());
        assert!(local_day_start("yesterday").is_none());
        assert!(local_day_start("2024-03").is_none());
    }

    #[test]
    fn unset_times_render_empty() {
        assert_eq!(format_time(0), "");
    }

    #[test]
    fn now_is_after_2020() {
        assert!(now_filetime() > local_day_start("2020-01-01").unwrap());
    }

    #[test]
    fn the_system_drive_reports_real_space() {
        let usage = disk_usage(Path::new(r"C:\")).unwrap();
        assert!(usage.total > 0 && usage.free <= usage.total);
        assert!((0.0..=1.0).contains(&usage.used_ratio()));
    }

    #[test]
    fn quick_access_opens_and_lists_named_absolute_paths() {
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        let pins = unsafe { enumerate_pins() }.unwrap();
        assert!(pins.iter().all(|(path, name)| path.is_absolute() && !name.is_empty()));
    }

    #[test]
    fn drives_include_the_system_drive() {
        assert!(drives().iter().any(|drive| drive.root == Path::new(r"C:\")));
    }
}
