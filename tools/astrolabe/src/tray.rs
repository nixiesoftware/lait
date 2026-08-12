//! The tray icon, and what closing the window actually means.
//!
//! Closing minimises here; the device keeps serving and its Spaces keep
//! converging. That is the whole reason this module exists: without somewhere to
//! minimise *to*, "close does not stop your daemon" is a thing that happens
//! invisibly, and a person who clicked the X has no way to get the window back.
//!
//! ## Why this is hand-written
//!
//! `eframe` owns the event loop and has no tray. Every crate that provides one
//! is another dependency in a closure that a build-failing licence audit and a
//! generated notice file both have to carry — for four Win32 calls and a menu.
//! The shape here is the standard one: a message-only window on its own thread,
//! `Shell_NotifyIconW` to place the icon, and a channel back to the frame loop.
//!
//! ## What is testable without a window
//!
//! The *policy* — what each menu entry means — is a pure function, and it is
//! the part that can be wrong in a way a person notices. "Exit" that took a
//! device offline because it was the first item in the menu is a defect;
//! `command_for` is where that is decided, and it is asserted below on every
//! platform.

use std::sync::mpsc::Receiver;

use crate::lifecycle::ExitRequest;

/// What a person asked for from the tray.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    /// Bring the window back.
    Restore,
    /// Leave, having been asked which kind of leaving this is.
    Exit(ExitRequest),
}

/// Menu command ids. Fixed rather than generated, so the pure policy below can
/// be tested against the same numbers the menu is built from.
pub const MENU_RESTORE: u32 = 1;
pub const MENU_EXIT_STAY_ONLINE: u32 = 2;
pub const MENU_EXIT_GO_OFFLINE: u32 = 3;

/// What one menu id means.
///
/// The two exits are separate entries rather than one "Exit" that then asks,
/// because the tray is where somebody leaves in a hurry — and a dialog nobody
/// reads is how "stop serving my Spaces" gets clicked by accident.
pub const fn command_for(id: u32) -> Option<TrayCommand> {
    match id {
        MENU_RESTORE => Some(TrayCommand::Restore),
        MENU_EXIT_STAY_ONLINE => Some(TrayCommand::Exit(ExitRequest::StayOnline)),
        MENU_EXIT_GO_OFFLINE => Some(TrayCommand::Exit(ExitRequest::GoOffline)),
        _ => None,
    }
}

/// The label each entry carries.
///
/// Spelled out rather than abbreviated: "Exit" alone cannot say which of the
/// two things it does, and the difference is whether somebody's Spaces keep
/// converging after they close a window.
pub const fn label_for(id: u32) -> &'static str {
    match id {
        MENU_RESTORE => "Open Astrolabe",
        MENU_EXIT_STAY_ONLINE => "Close and stay online",
        MENU_EXIT_GO_OFFLINE => "Go offline and exit",
        _ => "",
    }
}

/// The order the menu is built in.
pub const MENU: [u32; 3] = [MENU_RESTORE, MENU_EXIT_STAY_ONLINE, MENU_EXIT_GO_OFFLINE];

/// A placed tray icon. Dropping it removes the icon and ends its thread.
pub struct Tray {
    #[cfg(windows)]
    inner: imp::Tray,
}

impl Tray {
    /// Place the icon, and hand back the channel its menu talks through.
    ///
    /// Returns the receiver alongside the tray for the reason `Supervisor::start`
    /// returns its stream alongside the supervisor: a caller that could hold one
    /// without the other would have a window in which a click goes nowhere.
    ///
    /// On a platform with no tray this succeeds and produces a receiver that
    /// never yields. Windows is the v1 target; the client still builds and its
    /// tests still run everywhere, and a constructor that failed elsewhere would
    /// make that untrue for no benefit.
    pub fn place(tooltip: &str) -> anyhow::Result<(Self, Receiver<TrayCommand>)> {
        #[cfg(windows)]
        {
            let (inner, commands) = imp::Tray::place(tooltip)?;
            Ok((Self { inner }, commands))
        }
        #[cfg(not(windows))]
        {
            let _ = tooltip;
            let (_sender, commands) = std::sync::mpsc::channel();
            Ok((Self {}, commands))
        }
    }

    /// Show a notification from the tray.
    ///
    /// Best effort by design: a notification that could fail the operation that
    /// produced it would make "tell them something happened" a reason for the
    /// thing not to happen.
    pub fn notify(&self, title: &str, body: &str) {
        #[cfg(windows)]
        self.inner.notify(title, body);
        #[cfg(not(windows))]
        {
            let _ = (title, body);
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::{Arc, Mutex};

    use anyhow::{anyhow, Result};
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Shell::{
        Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
        NIM_MODIFY, NOTIFYICONDATAW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
        DispatchMessageW, GetCursorPos, GetMessageW, LoadIconW, PostMessageW, PostQuitMessage,
        RegisterClassW, SetForegroundWindow, TrackPopupMenu, TranslateMessage, HMENU,
        IDI_APPLICATION, MF_STRING, MSG, TPM_BOTTOMALIGN, TPM_RIGHTALIGN, WM_APP, WM_COMMAND,
        WM_DESTROY, WM_LBUTTONUP, WM_RBUTTONUP, WNDCLASSW, WS_OVERLAPPED,
    };

    /// The message the icon sends this window when it is clicked.
    const WM_TRAY: u32 = WM_APP + 1;
    /// Posted from another thread when there is a notification waiting.
    const WM_NOTIFY_PENDING: u32 = WM_APP + 2;
    /// Posted from `Drop` to end the pump.
    const WM_TRAY_QUIT: u32 = WM_APP + 3;
    const ICON_ID: u32 = 1;

    /// An `HWND` on its way to another thread.
    ///
    /// `PostMessageW` is documented as callable from any thread — posting is how
    /// threads talk to a window at all — so this is the one handle that crosses.
    /// Nothing else does: the window is created, pumped and destroyed on the
    /// thread that owns it.
    struct Posting(HWND);

    // SAFETY: the only use of the inner handle off the owning thread is
    // `PostMessageW`, which is thread-safe by contract.
    unsafe impl Send for Posting {}
    // SAFETY: as above; the handle is never dereferenced here.
    unsafe impl Sync for Posting {}

    /// What the pump thread needs to reach, shared with whoever holds the tray.
    struct Shared {
        window: Mutex<Option<Posting>>,
        pending: Mutex<Vec<(String, String)>>,
        commands: Sender<super::TrayCommand>,
    }

    pub(super) struct Tray {
        shared: Arc<Shared>,
        worker: Option<std::thread::JoinHandle<()>>,
    }

    impl Tray {
        pub(super) fn place(tooltip: &str) -> Result<(Self, Receiver<super::TrayCommand>)> {
            let (sender, commands) = channel();
            let shared = Arc::new(Shared {
                window: Mutex::new(None),
                pending: Mutex::new(Vec::new()),
                commands: sender,
            });
            let (ready, started) = channel();
            let tooltip = tooltip.to_owned();
            let pump_shared = Arc::clone(&shared);
            let worker = std::thread::Builder::new()
                .name("astrolabe-tray".into())
                .spawn(move || pump(&pump_shared, &tooltip, &ready))?;
            // Wait for the window to exist before handing the tray back: a
            // `notify` posted at a window that has not been created yet is a
            // notification that silently does not appear.
            match started.recv() {
                Ok(Ok(())) => Ok((
                    Self {
                        shared,
                        worker: Some(worker),
                    },
                    commands,
                )),
                Ok(Err(message)) => Err(anyhow!(message)),
                Err(_) => Err(anyhow!("the tray thread ended before it placed an icon")),
            }
        }

        pub(super) fn notify(&self, title: &str, body: &str) {
            if let Ok(mut pending) = self.shared.pending.lock() {
                pending.push((title.to_owned(), body.to_owned()));
            }
            self.post(WM_NOTIFY_PENDING);
        }

        fn post(&self, message: u32) {
            let Ok(window) = self.shared.window.lock() else {
                return;
            };
            let Some(window) = window.as_ref() else {
                return;
            };
            // SAFETY: posting to a window this process created; `PostMessageW`
            // is thread-safe and does not dereference the payload arguments.
            unsafe {
                PostMessageW(window.0, message, 0, 0);
            }
        }
    }

    impl Drop for Tray {
        fn drop(&mut self) {
            self.post(WM_TRAY_QUIT);
            if let Some(worker) = self.worker.take() {
                let _ = worker.join();
            }
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    /// Copy `text` into a fixed-width wide buffer, truncating rather than
    /// overrunning. The Win32 structures carry arrays, not pointers.
    fn fill(buffer: &mut [u16], text: &str) {
        let encoded: Vec<u16> = text
            .encode_utf16()
            .take(buffer.len().saturating_sub(1))
            .collect();
        for (slot, value) in buffer.iter_mut().zip(encoded.iter().copied()) {
            *slot = value;
        }
    }

    fn pump(shared: &Arc<Shared>, tooltip: &str, ready: &Sender<Result<(), String>>) {
        let class = wide("AstrolabeTray");
        // SAFETY: every pointer below is either null or a buffer that outlives
        // the call, and the window procedure is a plain `extern "system"` fn.
        let window = unsafe {
            let instance = GetModuleHandleW(std::ptr::null());
            let mut description: WNDCLASSW = std::mem::zeroed();
            description.lpfnWndProc = Some(procedure);
            description.hInstance = instance;
            description.lpszClassName = class.as_ptr();
            // A class that is already registered is not an error: this process
            // may place a tray more than once over its life.
            RegisterClassW(&raw const description);
            CreateWindowExW(
                0,
                class.as_ptr(),
                class.as_ptr(),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null(),
            )
        };
        if window.is_null() {
            let _ = ready.send(Err("could not create the tray's window".into()));
            return;
        }

        // The window procedure reaches the channel through this. Set before the
        // icon is placed, so the first click cannot arrive at an empty slot.
        set_context(window, Arc::clone(shared));

        let mut data = icon_data(window);
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = WM_TRAY;
        // SAFETY: a null instance with a system icon id is the documented way to
        // load a stock icon.
        data.hIcon = unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) };
        fill(&mut data.szTip, tooltip);
        // SAFETY: `data` is a fully initialised structure that outlives the call.
        let placed = unsafe { Shell_NotifyIconW(NIM_ADD, &raw const data) };
        if placed == 0 {
            // SAFETY: destroying a window this thread created.
            unsafe {
                DestroyWindow(window);
            }
            let _ = ready.send(Err("the shell refused a tray icon".into()));
            return;
        }

        if let Ok(mut held) = shared.window.lock() {
            *held = Some(Posting(window));
        }
        let _ = ready.send(Ok(()));

        // SAFETY: an ordinary message loop over a window this thread owns.
        unsafe {
            let mut message: MSG = std::mem::zeroed();
            while GetMessageW(&raw mut message, std::ptr::null_mut(), 0, 0) > 0 {
                TranslateMessage(&raw const message);
                DispatchMessageW(&raw const message);
            }
        }

        // SAFETY: removing the icon this thread placed.
        unsafe {
            let data = icon_data(window);
            Shell_NotifyIconW(NIM_DELETE, &raw const data);
        }
        clear_context(window);
        if let Ok(mut held) = shared.window.lock() {
            *held = None;
        }
    }

    fn icon_data(window: HWND) -> NOTIFYICONDATAW {
        // SAFETY: `NOTIFYICONDATAW` is a plain-old-data structure whose all-zero
        // state is the documented starting point.
        let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
        data.cbSize = u32::try_from(std::mem::size_of::<NOTIFYICONDATAW>()).unwrap_or(0);
        data.hWnd = window;
        data.uID = ICON_ID;
        data
    }

    /// The window procedure has no way to carry state, so the shared half is
    /// kept in a process-wide map keyed by window handle. One entry, in
    /// practice — a single-instance client places one tray.
    /// Window address to the state its procedure needs.
    type Contexts = Mutex<Vec<(usize, Arc<Shared>)>>;

    fn contexts() -> &'static Contexts {
        static CONTEXTS: std::sync::OnceLock<Contexts> = std::sync::OnceLock::new();
        CONTEXTS.get_or_init(|| Mutex::new(Vec::new()))
    }

    fn set_context(window: HWND, shared: Arc<Shared>) {
        if let Ok(mut contexts) = contexts().lock() {
            contexts.push((window.addr(), shared));
        }
    }

    fn clear_context(window: HWND) {
        if let Ok(mut contexts) = contexts().lock() {
            contexts.retain(|(held, _)| *held != window.addr());
        }
    }

    fn context(window: HWND) -> Option<Arc<Shared>> {
        let contexts = contexts().lock().ok()?;
        contexts
            .iter()
            .find(|(held, _)| *held == window.addr())
            .map(|(_, shared)| Arc::clone(shared))
    }

    extern "system" fn procedure(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_TRAY => {
                // The low word of `lparam` is the mouse message the icon saw.
                let event = u32::try_from(lparam & 0xffff).unwrap_or(0);
                match event {
                    WM_LBUTTONUP => send(window, super::TrayCommand::Restore),
                    WM_RBUTTONUP => show_menu(window),
                    _ => {}
                }
                0
            }
            WM_COMMAND => {
                let id = u32::try_from(wparam & 0xffff).unwrap_or(0);
                if let Some(command) = super::command_for(id) {
                    send(window, command);
                }
                0
            }
            WM_NOTIFY_PENDING => {
                drain_notifications(window);
                0
            }
            WM_TRAY_QUIT => {
                // SAFETY: destroying a window this thread owns; the resulting
                // `WM_DESTROY` is what ends the loop.
                unsafe {
                    DestroyWindow(window);
                }
                0
            }
            WM_DESTROY => {
                // SAFETY: ending this thread's own message loop.
                unsafe {
                    PostQuitMessage(0);
                }
                0
            }
            // SAFETY: the default handler for everything this window does not
            // interpret.
            _ => unsafe { DefWindowProcW(window, message, wparam, lparam) },
        }
    }

    fn send(window: HWND, command: super::TrayCommand) {
        if let Some(shared) = context(window) {
            // A closed receiver means the client is already going away. Not an
            // error, and nothing to report to.
            let _ = shared.commands.send(command);
        }
    }

    fn show_menu(window: HWND) {
        // SAFETY: an ordinary popup menu built and torn down within this call.
        // `SetForegroundWindow` before `TrackPopupMenu` is what the API requires
        // for the menu to dismiss when a person clicks elsewhere.
        unsafe {
            let menu: HMENU = CreatePopupMenu();
            if menu.is_null() {
                return;
            }
            for id in super::MENU {
                let label = wide(super::label_for(id));
                AppendMenuW(menu, MF_STRING, id as usize, label.as_ptr());
            }
            let mut cursor = POINT { x: 0, y: 0 };
            GetCursorPos(&raw mut cursor);
            SetForegroundWindow(window);
            TrackPopupMenu(
                menu,
                TPM_RIGHTALIGN | TPM_BOTTOMALIGN,
                cursor.x,
                cursor.y,
                0,
                window,
                std::ptr::null(),
            );
            DestroyMenu(menu);
        }
    }

    fn drain_notifications(window: HWND) {
        let Some(shared) = context(window) else {
            return;
        };
        let waiting: Vec<(String, String)> = match shared.pending.lock() {
            Ok(mut pending) => std::mem::take(&mut pending),
            Err(_) => return,
        };
        for (title, body) in waiting {
            let mut data = icon_data(window);
            data.uFlags = NIF_INFO;
            fill(&mut data.szInfoTitle, &title);
            fill(&mut data.szInfo, &body);
            // SAFETY: `data` is fully initialised and outlives the call.
            unsafe {
                Shell_NotifyIconW(NIM_MODIFY, &raw const data);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two exits are different acts, and the tray is where they are most
    /// likely to be clicked without reading. Nothing may collapse them.
    #[test]
    fn the_menu_never_hides_which_kind_of_leaving_it_means() {
        assert_eq!(command_for(MENU_RESTORE), Some(TrayCommand::Restore));
        assert_eq!(
            command_for(MENU_EXIT_STAY_ONLINE),
            Some(TrayCommand::Exit(ExitRequest::StayOnline))
        );
        assert_eq!(
            command_for(MENU_EXIT_GO_OFFLINE),
            Some(TrayCommand::Exit(ExitRequest::GoOffline))
        );
        assert_eq!(command_for(0), None, "an unknown id became a command");
        assert_eq!(command_for(99), None);

        for id in MENU {
            let label = label_for(id);
            assert!(!label.is_empty(), "menu entry {id} has no label");
        }
        // Neither exit is spelled as a bare "Exit": the word alone cannot say
        // whether somebody's Spaces keep converging afterwards.
        assert_ne!(label_for(MENU_EXIT_STAY_ONLINE), "Exit");
        assert_ne!(label_for(MENU_EXIT_GO_OFFLINE), "Exit");
        assert!(label_for(MENU_EXIT_GO_OFFLINE).contains("offline"));
        assert!(label_for(MENU_EXIT_STAY_ONLINE).contains("online"));
    }

    /// Restore is first. Somebody who minimised by accident reaches for the
    /// first entry, and the first entry must not be one that ends anything.
    #[test]
    fn the_first_entry_is_the_harmless_one() {
        assert_eq!(MENU.first().copied(), Some(MENU_RESTORE));
    }
}
