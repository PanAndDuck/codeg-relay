//! GUI-subsystem processes have no console; loading user32 also suppresses
//! console logoff/shutdown notifications. A hidden top-level window receives
//! WM_ENDSESSION in both binaries. Console Ctrl-C/Break/Close still use Tokio.
//! https://learn.microsoft.com/windows/console/setconsolectrlhandler
use anyhow::Context;
use std::{
    io,
    ptr::{null, null_mut},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};
use tokio::sync::oneshot;
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, LRESULT, WPARAM},
    System::LibraryLoader::GetModuleHandleW,
    UI::WindowsAndMessaging::*,
};

pub(crate) struct SessionEnd {
    window: usize,
    event: oneshot::Receiver<()>,
    completed: Option<mpsc::SyncSender<()>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl SessionEnd {
    pub(crate) fn new() -> anyhow::Result<Self> {
        let (event_tx, event) = oneshot::channel();
        let (completed, completed_rx) = mpsc::sync_channel(1);
        let (ready_tx, ready) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("session-notifications".into())
            .spawn(move || {
                if let Err(error) = window_loop(event_tx, completed_rx, &ready_tx) {
                    let _ = ready_tx.send(Err(error));
                }
            })?;
        let window = ready
            .recv()
            .context("session notification thread stopped")??;
        Ok(Self {
            window,
            event,
            completed: Some(completed),
            worker: Some(worker),
        })
    }

    pub(crate) async fn notified(&mut self) -> anyhow::Result<()> {
        (&mut self.event)
            .await
            .context("session notification window closed unexpectedly")
    }
}

impl Drop for SessionEnd {
    fn drop(&mut self) {
        // run() keeps this guard until the agent has drained and saved status.
        // Release WM_ENDSESSION before asking the owning thread to destroy HWND.
        if let Some(completed) = self.completed.take() {
            let _ = completed.send(());
        }
        // SAFETY: the owning thread keeps HWND alive until processing WM_CLOSE;
        // posting to an already closed handle simply fails.
        unsafe {
            PostMessageW(self.window as HWND, WM_CLOSE, 0, 0);
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct WindowState {
    event: Option<oneshot::Sender<()>>,
    completed: mpsc::Receiver<()>,
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_QUERYENDSESSION => 1,
        WM_ENDSESSION if wparam != 0 => {
            // SAFETY: window_loop sets this pointer before pumping messages;
            // its Box stays alive until after DestroyWindow, on this thread.
            let state = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut WindowState;
            if let Some(state) = state.as_mut() {
                if let Some(event) = state.event.take() {
                    let _ = event.send(());
                    // Returning from WM_ENDSESSION permits OS termination. Give
                    // async cleanup a bounded window; never delay logout forever.
                    let _ = state.completed.recv_timeout(Duration::from_secs(4));
                }
            }
            0
        }
        WM_CLOSE => {
            DestroyWindow(window);
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}

fn window_loop(
    event: oneshot::Sender<()>,
    completed: mpsc::Receiver<()>,
    ready: &mpsc::SyncSender<io::Result<usize>>,
) -> io::Result<()> {
    static NEXT_CLASS: AtomicU64 = AtomicU64::new(0);
    let class_name: Vec<u16> = format!(
        "codeg-relay-agent-session-{}",
        NEXT_CLASS.fetch_add(1, Ordering::Relaxed)
    )
    .encode_utf16()
    .chain(Some(0))
    .collect();
    let mut state = Box::new(WindowState {
        event: Some(event),
        completed,
    });
    // SAFETY: all Win32 structs and strings remain live for their calls. HWND
    // creation, dispatch, and destruction happen exclusively on this thread.
    unsafe {
        let instance = GetModuleHandleW(null());
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..std::mem::zeroed()
        };
        if RegisterClassW(&class) == 0 {
            return Err(io::Error::last_os_error());
        }
        // No WS_VISIBLE, no ShowWindow, and no HWND_MESSAGE: only a hidden
        // top-level window receives the session-ending broadcast.
        let window = CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            null(),
        );
        if window.is_null() {
            let error = io::Error::last_os_error();
            UnregisterClassW(class_name.as_ptr(), instance);
            return Err(error);
        }
        SetWindowLongPtrW(
            window,
            GWLP_USERDATA,
            state.as_mut() as *mut WindowState as isize,
        );
        let _ = ready.send(Ok(window as usize));
        let mut message: MSG = std::mem::zeroed();
        let result = loop {
            match GetMessageW(&mut message, null_mut(), 0, 0) {
                -1 => break Err(io::Error::last_os_error()),
                0 => break Ok(()),
                _ => {
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        };
        if IsWindow(window) != 0 {
            DestroyWindow(window);
        }
        UnregisterClassW(class_name.as_ptr(), instance);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn session_end_notifies_without_a_visible_window() {
        let mut listener = SessionEnd::new().unwrap();
        // SAFETY: target only the test's own HWND; no real logout is requested.
        unsafe {
            assert_eq!(IsWindowVisible(listener.window as HWND), 0);
            assert_ne!(
                PostMessageW(listener.window as HWND, WM_ENDSESSION, 1, 0),
                0
            );
        }
        tokio::time::timeout(Duration::from_secs(2), listener.notified())
            .await
            .unwrap()
            .unwrap();
        // Drop acknowledges async cleanup and joins the message thread.
    }
}
