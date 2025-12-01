//! Application state and logic for the TUI file browser

use anyhow::Result;
use fatrs::{FileSystem, OemCpConverter, ReadWriteSeek, TimeProvider};
use ratatui::widgets::ListState;
use tokio::runtime::Handle;

/// Current view mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub enum View {
    Browser,
    FileContent,
    HexView,
    ImageView,
    Help,
}

/// Input mode for the application
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    Input,
}

/// Input action being performed
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputAction {
    CreateFile,
    CreateDir,
    Rename,
    Export,
}

/// Represents a file or directory entry
#[derive(Debug, Clone)]
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: String,
}

/// Application state
pub struct App<IO: ReadWriteSeek, TP: TimeProvider, OCC: OemCpConverter>
where
    IO::Error: 'static,
{
    /// The FAT filesystem
    pub fs: FileSystem<IO, TP, OCC>,
    /// Tokio runtime handle
    pub runtime: Handle,
    /// Whether mounted read-only
    pub read_only: bool,
    /// Current directory path components
    pub current_path: Vec<String>,
    /// Directory entries
    pub entries: Vec<FileEntry>,
    /// List widget state
    pub list_state: ListState,
    /// Current view
    pub view: View,
    /// Input mode
    pub input_mode: InputMode,
    /// Input action
    pub input_action: Option<InputAction>,
    /// Input prompt
    pub input_prompt: String,
    /// Input buffer
    pub input_buffer: String,
    /// File content for viewing
    pub file_content: Option<String>,
    /// File bytes for hex view
    pub file_bytes: Option<Vec<u8>>,
    /// Currently viewing file name
    pub viewing_file: Option<String>,
    /// Scroll offset for content viewing
    pub scroll_offset: usize,
    /// Total lines in content
    pub total_lines: usize,
    /// Image data for image view
    pub image_data: Option<image::DynamicImage>,
    /// Message to display
    pub message: Option<String>,
}

impl<IO: ReadWriteSeek, TP: TimeProvider, OCC: OemCpConverter> App<IO, TP, OCC>
where
    IO::Error: std::error::Error + 'static,
{
    /// Create a new application
    pub fn new(fs: FileSystem<IO, TP, OCC>, runtime: Handle, read_only: bool) -> Result<Self> {
        let mut list_state = ListState::default();
        list_state.select(Some(0));

        Ok(Self {
            fs,
            runtime,
            read_only,
            current_path: Vec::new(),
            entries: Vec::new(),
            list_state,
            view: View::Browser,
            input_mode: InputMode::Normal,
            input_action: None,
            input_prompt: String::new(),
            input_buffer: String::new(),
            file_content: None,
            file_bytes: None,
            viewing_file: None,
            scroll_offset: 0,
            total_lines: 0,
            image_data: None,
            message: None,
        })
    }

    /// Load the current directory contents
    pub fn load_current_directory(&mut self) -> Result<()> {
        self.entries.clear();

        let path = self.current_path.join("/");

        let entries_result = self.runtime.block_on(async {
            let root = self.fs.root_dir();

            // Open the target directory
            let dir = if self.current_path.is_empty() {
                root
            } else {
                root.open_dir(&path).await?
            };

            // Collect entries
            let mut entries = Vec::new();
            let mut iter = dir.iter();
            while let Some(entry_result) = iter.next().await {
                match entry_result {
                    Ok(entry) => {
                        let name = entry.file_name();
                        // Skip . and .. entries
                        if name.as_str() == "." || name.as_str() == ".." {
                            continue;
                        }

                        let modified = entry.modified();
                        let modified_str = format!(
                            "{:04}-{:02}-{:02} {:02}:{:02}",
                            modified.date.year,
                            modified.date.month,
                            modified.date.day,
                            modified.time.hour,
                            modified.time.min
                        );

                        entries.push(FileEntry {
                            name: name.to_string(),
                            is_dir: entry.is_dir(),
                            size: entry.len(),
                            modified: modified_str,
                        });
                    }
                    Err(e) => return Err(e),
                }
            }

            Ok::<Vec<FileEntry>, fatrs::Error<IO::Error>>(entries)
        });

        match entries_result {
            Ok(mut entries) => {
                // Sort: directories first, then by name
                entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
                    (true, false) => std::cmp::Ordering::Less,
                    (false, true) => std::cmp::Ordering::Greater,
                    _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                });
                self.entries = entries;

                // Reset selection
                if !self.entries.is_empty() {
                    self.list_state.select(Some(0));
                } else {
                    self.list_state.select(None);
                }
            }
            Err(e) => {
                self.message = Some(format!("Error loading directory: {:?}", e));
            }
        }

        Ok(())
    }

    /// Move selection to next item
    pub fn next(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => {
                if i >= self.entries.len() - 1 {
                    0
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    /// Move selection to previous item
    pub fn previous(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        let i = match self.list_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.entries.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.list_state.select(Some(i));
    }

    /// Get the currently selected entry
    pub fn get_selected_entry(&self) -> Option<&FileEntry> {
        self.list_state.selected().and_then(|i| self.entries.get(i))
    }

    /// Enter the selected item (directory or view file)
    pub fn enter_selected(&mut self) -> Result<()> {
        if let Some(entry) = self.get_selected_entry().cloned() {
            if entry.is_dir {
                // Clear any viewed content when entering a directory
                self.clear_view_data();
                self.view = View::Browser;
                self.current_path.push(entry.name);
                self.load_current_directory()?;
            } else {
                self.view_file()?;
            }
        }
        Ok(())
    }

    /// Go to parent directory
    pub fn go_parent(&mut self) -> Result<()> {
        if !self.current_path.is_empty() {
            // Clear any viewed content when going to parent
            self.clear_view_data();
            self.view = View::Browser;
            self.current_path.pop();
            self.load_current_directory()?;
        }
        Ok(())
    }

    /// View the selected file as text
    pub fn view_file(&mut self) -> Result<()> {
        if let Some(entry) = self.get_selected_entry().cloned() {
            if entry.is_dir {
                return Ok(());
            }

            let path = if self.current_path.is_empty() {
                entry.name.clone()
            } else {
                format!("{}/{}", self.current_path.join("/"), entry.name)
            };

            let content_result = self.runtime.block_on(async {
                use embedded_io_async::Read;

                let root = self.fs.root_dir();
                let mut file = root.open_file(&path).await?;

                let mut buffer = Vec::new();
                let mut chunk = [0u8; 4096];

                loop {
                    let n = file.read(&mut chunk).await?;
                    if n == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..n]);

                    // Limit to 1MB for viewing
                    if buffer.len() > 1024 * 1024 {
                        break;
                    }
                }

                Ok::<Vec<u8>, fatrs::Error<IO::Error>>(buffer)
            });

            match content_result {
                Ok(bytes) => {
                    self.file_bytes = Some(bytes.clone());

                    // Check if this is an image file
                    let filename_lower = entry.name.to_lowercase();
                    let is_image = filename_lower.ends_with(".jpg")
                        || filename_lower.ends_with(".jpeg")
                        || filename_lower.ends_with(".png")
                        || filename_lower.ends_with(".gif")
                        || filename_lower.ends_with(".bmp");

                    if is_image {
                        // Try to decode as image
                        match image::load_from_memory(&bytes) {
                            Ok(img) => {
                                self.image_data = Some(img);
                                self.viewing_file = Some(entry.name);
                                self.scroll_offset = 0;
                                self.view = View::ImageView;
                            }
                            Err(e) => {
                                // If image decode fails, show as text
                                self.file_content =
                                    Some(String::from_utf8_lossy(&bytes).to_string());
                                self.viewing_file = Some(entry.name);
                                self.scroll_offset = 0;
                                self.total_lines = self
                                    .file_content
                                    .as_ref()
                                    .map(|c| c.lines().count())
                                    .unwrap_or(0);
                                self.view = View::FileContent;
                                self.message = Some(format!("Failed to decode image: {:?}", e));
                            }
                        }
                    } else {
                        // Regular text file
                        self.file_content = Some(String::from_utf8_lossy(&bytes).to_string());
                        self.viewing_file = Some(entry.name);
                        self.scroll_offset = 0;
                        self.total_lines = self
                            .file_content
                            .as_ref()
                            .map(|c| c.lines().count())
                            .unwrap_or(0);
                        self.view = View::FileContent;
                    }
                }
                Err(e) => {
                    self.message = Some(format!("Error reading file: {:?}", e));
                }
            }
        }
        Ok(())
    }

    /// Toggle hex view
    pub fn toggle_hex_view(&mut self) {
        if self.file_bytes.is_some() {
            self.scroll_offset = 0;
            self.view = match self.view {
                View::HexView => View::FileContent,
                _ => View::HexView,
            };
        } else if let Some(entry) = self.get_selected_entry().cloned() {
            if !entry.is_dir {
                // Load file first
                let _ = self.view_file();
                self.view = View::HexView;
            }
        }
    }

    /// Toggle help view
    pub fn toggle_help(&mut self) {
        self.view = match self.view {
            View::Help => View::Browser,
            _ => View::Help,
        };
    }

    /// Clear view data when returning to browser
    pub fn clear_view_data(&mut self) {
        self.file_content = None;
        self.file_bytes = None;
        self.image_data = None;
        self.viewing_file = None;
        self.scroll_offset = 0;
        self.total_lines = 0;
    }

    /// Scroll up in content view
    pub fn scroll_up(&mut self, amount: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
    }

    /// Scroll down in content view
    pub fn scroll_down(&mut self, amount: usize) {
        let max_scroll = match self.view {
            View::HexView => self.file_bytes.as_ref().map(|b| b.len() / 16).unwrap_or(0),
            _ => self.total_lines,
        };
        self.scroll_offset = (self.scroll_offset + amount).min(max_scroll.saturating_sub(1));
    }

    /// Scroll to top
    pub fn scroll_to_top(&mut self) {
        self.scroll_offset = 0;
    }

    /// Scroll to bottom
    pub fn scroll_to_bottom(&mut self) {
        let max_scroll = match self.view {
            View::HexView => self.file_bytes.as_ref().map(|b| b.len() / 16).unwrap_or(0),
            _ => self.total_lines,
        };
        self.scroll_offset = max_scroll.saturating_sub(1);
    }

    /// Start creating a new file
    pub fn start_create_file(&mut self) {
        if self.read_only {
            self.message = Some("Cannot create file: mounted read-only".to_string());
            return;
        }
        self.input_mode = InputMode::Input;
        self.input_action = Some(InputAction::CreateFile);
        self.input_prompt = "New file name: ".to_string();
        self.input_buffer.clear();
    }

    /// Start creating a new directory
    pub fn start_create_dir(&mut self) {
        if self.read_only {
            self.message = Some("Cannot create directory: mounted read-only".to_string());
            return;
        }
        self.input_mode = InputMode::Input;
        self.input_action = Some(InputAction::CreateDir);
        self.input_prompt = "New directory name: ".to_string();
        self.input_buffer.clear();
    }

    /// Start renaming selected item
    pub fn start_rename(&mut self) {
        if self.read_only {
            self.message = Some("Cannot rename: mounted read-only".to_string());
            return;
        }
        let entry_info = self.get_selected_entry().map(|e| e.name.clone());
        if let Some(name) = entry_info {
            self.input_mode = InputMode::Input;
            self.input_action = Some(InputAction::Rename);
            self.input_prompt = format!("Rename '{}' to: ", name);
            self.input_buffer = name;
        }
    }

    /// Start exporting selected file to local disk
    pub fn start_export(&mut self) {
        let entry_info = self
            .get_selected_entry()
            .map(|e| (e.name.clone(), e.is_dir));
        if let Some((name, is_dir)) = entry_info {
            if is_dir {
                self.message =
                    Some("Cannot export directories (yet). Use CLI: fatrs flash cp".to_string());
                return;
            }
            self.input_mode = InputMode::Input;
            self.input_action = Some(InputAction::Export);
            self.input_prompt = format!("Export '{}' to: ", name);
            // Default to current directory with same filename
            self.input_buffer = format!("./{}", name);
        }
    }

    /// Cancel input mode
    pub fn cancel_input(&mut self) {
        self.input_mode = InputMode::Normal;
        self.input_action = None;
        self.input_buffer.clear();
    }

    /// Confirm input and perform action
    pub fn confirm_input(&mut self) -> Result<()> {
        let action = self.input_action.take();
        let name = std::mem::take(&mut self.input_buffer);
        self.input_mode = InputMode::Normal;

        if name.is_empty() {
            return Ok(());
        }

        match action {
            Some(InputAction::CreateFile) => self.create_file(&name)?,
            Some(InputAction::CreateDir) => self.create_dir(&name)?,
            Some(InputAction::Rename) => self.rename_selected(&name)?,
            Some(InputAction::Export) => self.export_file(&name)?,
            None => {}
        }

        Ok(())
    }

    /// Create a new file
    fn create_file(&mut self, name: &str) -> Result<()> {
        let path = if self.current_path.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", self.current_path.join("/"), name)
        };

        let name_owned = name.to_string();
        let result: Result<(), String> = self.runtime.block_on(async {
            let root = self.fs.root_dir();
            root.create_file(&path)
                .await
                .map(|_| ())
                .map_err(|e| format!("{:?}", e))
        });

        match result {
            Ok(_) => {
                self.message = Some(format!("Created file: {}", name_owned));
            }
            Err(e) => {
                self.message = Some(format!("Error creating file: {}", e));
            }
        }

        self.load_current_directory()?;
        Ok(())
    }

    /// Create a new directory
    fn create_dir(&mut self, name: &str) -> Result<()> {
        let path = if self.current_path.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", self.current_path.join("/"), name)
        };

        let name_owned = name.to_string();
        let result: Result<(), String> = self.runtime.block_on(async {
            let root = self.fs.root_dir();
            root.create_dir(&path)
                .await
                .map(|_| ())
                .map_err(|e| format!("{:?}", e))
        });

        match result {
            Ok(_) => {
                self.message = Some(format!("Created directory: {}", name_owned));
            }
            Err(e) => {
                self.message = Some(format!("Error creating directory: {}", e));
            }
        }

        self.load_current_directory()?;
        Ok(())
    }

    /// Delete the selected item
    pub fn delete_selected(&mut self) -> Result<()> {
        if self.read_only {
            self.message = Some("Cannot delete: mounted read-only".to_string());
            return Ok(());
        }

        let entry_info = self.get_selected_entry().cloned();
        if let Some(entry) = entry_info {
            let path = if self.current_path.is_empty() {
                entry.name.clone()
            } else {
                format!("{}/{}", self.current_path.join("/"), entry.name)
            };

            let entry_name = entry.name.clone();
            let result: Result<(), String> = self.runtime.block_on(async {
                let root = self.fs.root_dir();
                root.remove(&path).await.map_err(|e| format!("{:?}", e))
            });

            match result {
                Ok(_) => {
                    self.message = Some(format!("Deleted: {}", entry_name));
                }
                Err(e) => {
                    self.message = Some(format!("Error deleting: {}", e));
                }
            }

            self.load_current_directory()?;
        }

        Ok(())
    }

    /// Rename the selected item
    fn rename_selected(&mut self, new_name: &str) -> Result<()> {
        let entry_info = self.get_selected_entry().cloned();
        if let Some(entry) = entry_info {
            let old_path = if self.current_path.is_empty() {
                entry.name.clone()
            } else {
                format!("{}/{}", self.current_path.join("/"), entry.name)
            };

            let new_path = if self.current_path.is_empty() {
                new_name.to_string()
            } else {
                format!("{}/{}", self.current_path.join("/"), new_name)
            };

            let entry_name = entry.name.clone();
            let new_name_owned = new_name.to_string();
            let result: Result<(), String> = self.runtime.block_on(async {
                let root = self.fs.root_dir();
                root.rename(&old_path, &root, &new_path)
                    .await
                    .map_err(|e| format!("{:?}", e))
            });

            match result {
                Ok(_) => {
                    self.message =
                        Some(format!("Renamed '{}' to '{}'", entry_name, new_name_owned));
                }
                Err(e) => {
                    self.message = Some(format!("Error renaming: {}", e));
                }
            }

            self.load_current_directory()?;
        }

        Ok(())
    }

    /// Export/copy a file to local disk
    fn export_file(&mut self, dest_path: &str) -> Result<()> {
        let entry_info = self.get_selected_entry().cloned();
        if let Some(entry) = entry_info {
            if entry.is_dir {
                self.message =
                    Some("Cannot export directories (use CLI for recursive copy)".to_string());
                return Ok(());
            }

            let source_path = if self.current_path.is_empty() {
                entry.name.clone()
            } else {
                format!("{}/{}", self.current_path.join("/"), entry.name)
            };

            let entry_name = entry.name.clone();
            let dest_path_owned = dest_path.to_string();

            let result: Result<(), String> = self.runtime.block_on(async {
                use embedded_io_async::Read;
                use std::io::Write;

                // Read file from FAT filesystem
                let root = self.fs.root_dir();
                let mut file = root
                    .open_file(&source_path)
                    .await
                    .map_err(|e| format!("Failed to open source: {:?}", e))?;

                // Create destination file
                let mut dest_file = std::fs::File::create(&dest_path_owned)
                    .map_err(|e| format!("Failed to create destination: {:?}", e))?;

                // Use 512KB chunks for optimal performance
                const CHUNK_SIZE: usize = 512 * 1024;
                let mut chunk = vec![0u8; CHUNK_SIZE];
                let mut total_bytes = 0usize;

                loop {
                    let n = file.read(&mut chunk).await.map_err(|e| {
                        format!("Failed to read at offset {}: {:?}", total_bytes, e)
                    })?;
                    if n == 0 {
                        break;
                    }

                    // Write incrementally to avoid accumulating everything in memory
                    dest_file.write_all(&chunk[..n]).map_err(|e| {
                        format!("Failed to write at offset {}: {:?}", total_bytes, e)
                    })?;

                    total_bytes += n;
                }

                // Ensure all data is flushed to disk
                dest_file
                    .flush()
                    .map_err(|e| format!("Failed to flush: {:?}", e))?;

                Ok(())
            });

            match result {
                Ok(_) => {
                    self.message = Some(format!(
                        "Exported '{}' to '{}'",
                        entry_name, dest_path_owned
                    ));
                }
                Err(e) => {
                    self.message = Some(format!("Export failed: {}", e));
                }
            }
        }

        Ok(())
    }
}
