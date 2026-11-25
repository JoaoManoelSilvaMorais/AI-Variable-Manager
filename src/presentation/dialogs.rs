use std::path::{Path, PathBuf};

use rfd::FileDialog;

use crate::shared::error::{AppError, AppResult};

pub fn pick_spreadsheet_file() -> Option<PathBuf> {
    FileDialog::new()
        .set_title("Selecionar planilha (.xlsx ou .csv)")
        .add_filter("Planilhas", &["xlsx", "csv"])
        .pick_file()
}

pub fn pick_xml_template_file() -> Option<PathBuf> {
    FileDialog::new()
        .set_title("Selecionar XML-base do Adobe Illustrator")
        .add_filter("XML do Illustrator", &["xml"])
        .pick_file()
}

pub fn pick_image_file(start_dir: Option<&Path>) -> Option<PathBuf> {
    let mut dialog = FileDialog::new()
        .set_title("Selecionar fotografia para vínculo")
        .add_filter("Imagens suportadas", &["jpg", "jpeg", "png", "tif", "tiff"]);
    if let Some(dir) = start_dir.filter(|p| p.is_dir()) {
        dialog = dialog.set_directory(dir);
    }
    dialog.pick_file()
}

pub fn pick_folder(title: &str, start_dir: Option<&Path>) -> Option<PathBuf> {
    let mut dialog = FileDialog::new().set_title(title);
    if let Some(dir) = start_dir.filter(|p| p.is_dir()) {
        dialog = dialog.set_directory(dir);
    }
    dialog.pick_folder()
}

/// Reads Unicode text (`CF_UNICODETEXT`) directly from the native Windows clipboard.
#[cfg(windows)]
pub fn read_clipboard_text() -> AppResult<String> {
    use core::ffi::c_void;

    const CF_UNICODETEXT: u32 = 13;

    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenClipboard(h_wnd_new_owner: *mut c_void) -> i32;
        fn CloseClipboard() -> i32;
        fn IsClipboardFormatAvailable(format: u32) -> i32;
        fn GetClipboardData(u_format: u32) -> *mut c_void;
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalLock(h_mem: *mut c_void) -> *const u16;
        fn GlobalUnlock(h_mem: *mut c_void) -> i32;
    }

    struct ClipboardGuard;
    impl Drop for ClipboardGuard {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }

    unsafe {
        if OpenClipboard(core::ptr::null_mut()) == 0 {
            return Err(AppError::validation(
                "Não foi possível abrir a área de transferência do Windows no momento.",
            ));
        }
        let _guard = ClipboardGuard;

        if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 {
            return Err(AppError::validation(
                "A área de transferência não contém texto tabular.",
            ));
        }

        let handle = GetClipboardData(CF_UNICODETEXT);
        if handle.is_null() {
            return Err(AppError::validation(
                "Não foi possível ler os dados da área de transferência.",
            ));
        }

        let ptr = GlobalLock(handle);
        if ptr.is_null() {
            return Err(AppError::validation(
                "Não foi possível acessar o bloco de texto da área de transferência.",
            ));
        }

        let mut len = 0usize;
        while *ptr.add(len) != 0 {
            len += 1;
        }
        let slice = core::slice::from_raw_parts(ptr, len);
        let text = String::from_utf16_lossy(slice);
        GlobalUnlock(handle);
        Ok(text)
    }
}

#[cfg(not(windows))]
pub fn read_clipboard_text() -> AppResult<String> {
    Err(AppError::validation(
        "Leitura automática da área de transferência disponível apenas no Windows; cole no campo de texto.",
    ))
}
