pub mod clipboard;
pub mod csv;
pub mod reader;
pub mod xlsx;

pub use clipboard::parse_clipboard_block;
pub use reader::CompositeSpreadsheetReader;
