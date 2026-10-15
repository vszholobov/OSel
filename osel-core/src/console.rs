//! Text console drawn into a linear framebuffer
//!
//! Glyphs come from the OSel Console bitmap font. Text goes from the top down,
//! once the cursor reaches the bottom row the screen scrolls up on every new line.

use bootloader_api::info::{FrameBufferInfo, PixelFormat};
use core::fmt;
use alloc::collections::TryReserveError;
use alloc::vec::Vec;
use crate::font::Font;

/// Width of the small font's text cell in pixels.
pub const CHAR_WIDTH: usize = Font::Small.width();
/// Height of the small font's cell, including the font's own line spacing.
pub const LINE_HEIGHT: usize = Font::Small.height();
/// Tab stops are every `TAB_WIDTH` columns
pub const TAB_WIDTH: usize = 8;
/// Scrolled-off rows retained in ordinary RAM.
pub const SCROLLBACK_LINES: usize = 1024;
/// Drawn for characters missing in the font
pub const UNKNOWN_CHAR: char = '\u{FFFD}';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const BLACK: Color = Color::new(0x00, 0x00, 0x00);
    pub const GREEN: Color = Color::new(0x00, 0xaa, 0x00);
    pub const WHITE: Color = Color::new(0xff, 0xff, 0xff);
    pub const RED: Color = Color::new(0xaa, 0x00, 0x00);

    pub const fn new(r: u8, g: u8, b: u8) -> Color {
        Color { r, g, b }
    }

    /// Mixes `self` (background) with `fg` using glyph pixel intensity 0..=255
    pub fn blend(self, fg: Color, intensity: u8) -> Color {
        let mix = |bg: u8, fg: u8| {
            ((bg as u16 * (255 - intensity as u16) + fg as u16 * intensity as u16) / 255) as u8
        };
        Color::new(mix(self.r, fg.r), mix(self.g, fg.g), mix(self.b, fg.b))
    }
}

/// Why a framebuffer can't be used as a console
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleError {
    /// Integer pixel scaling supports 1x, 2x and 3x.
    UnsupportedScale,
    /// Only RGB, BGR and grayscale framebuffers are supported
    UnsupportedPixelFormat(PixelFormat),
    /// Too few bytes per pixel for the pixel format, or more than 4
    UnsupportedPixelSize(usize),
    /// Stride smaller than width
    InvalidStride,
    /// Not even a single text cell fits on the screen
    ScreenTooSmall,
    /// The buffer is shorter than `stride * height * bytes_per_pixel`
    BufferTooSmall,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cell {
    character: char,
    foreground: Color,
    background: Color,
}

impl Cell {
    fn blank(background: Color) -> Self {
        Self { character: ' ', foreground: background, background }
    }
}

#[derive(Clone, Copy)]
struct ScreenState {
    row: usize,
    column: usize,
    foreground: Color,
    background: Color,
    cursor_visible: bool,
    bottom_color: Color,
}

struct TextBuffer {
    cells: Vec<Cell>,
    displayed: Vec<Option<Cell>>,
    bottom_color: Color,
    bottom_dirty: bool,
    history: Vec<Cell>,
    history_head: usize,
    history_len: usize,
    view_offset: usize,
    saved_cells: Vec<Cell>,
    saved_state: Option<ScreenState>,
}

pub struct Console<'a> {
    buffer: &'a mut [u8],
    text: Option<TextBuffer>,
    info: FrameBufferInfo,
    columns: usize,
    rows: usize,
    row: usize,
    column: usize,
    foreground: Color,
    background: Color,
    escape: Escape,
    cursor_visible: bool,
    scale: usize,
    font: Font,
}

#[derive(Clone, Copy)]
enum Escape {
    Text,
    Start,
    Csi {
        params: [u16; 8],
        index: usize,
        private: bool,
        invalid: bool,
    },
}

impl fmt::Write for Console<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

impl<'a> Console<'a> {
    /// Checks the framebuffer geometry and clears the screen
    pub fn new(buffer: &'a mut [u8], info: FrameBufferInfo) -> Result<Self, ConsoleError> {
        Self::new_scaled(buffer, info, 1)
    }

    /// Enlarges each font pixel to a `scale` by `scale` block without allocation.
    pub fn new_scaled(buffer: &'a mut [u8], info: FrameBufferInfo, scale: usize) -> Result<Self, ConsoleError> {
        Self::with_font_and_scale(buffer, info, Font::Small, scale)
    }

    /// Uses a native raster instead of enlarging the small font.
    pub fn new_with_font(buffer: &'a mut [u8], info: FrameBufferInfo, font: Font) -> Result<Self, ConsoleError> {
        Self::with_font_and_scale(buffer, info, font, 1)
    }

    fn with_font_and_scale(buffer: &'a mut [u8], info: FrameBufferInfo, font: Font, scale: usize) -> Result<Self, ConsoleError> {
        if !(1..=3).contains(&scale) {
            return Err(ConsoleError::UnsupportedScale);
        }
        let min_bytes_per_pixel = match info.pixel_format {
            PixelFormat::Rgb | PixelFormat::Bgr => 3,
            PixelFormat::U8 => 1,
            other => return Err(ConsoleError::UnsupportedPixelFormat(other)),
        };
        if info.bytes_per_pixel < min_bytes_per_pixel || info.bytes_per_pixel > 4 {
            return Err(ConsoleError::UnsupportedPixelSize(info.bytes_per_pixel));
        }
        if info.stride < info.width {
            return Err(ConsoleError::InvalidStride);
        }
        let (columns, rows) = (info.width / (font.width() * scale), info.height / (font.height() * scale));
        if columns == 0 || rows == 0 {
            return Err(ConsoleError::ScreenTooSmall);
        }
        let needed = info.stride.checked_mul(info.height).and_then(|p| p.checked_mul(info.bytes_per_pixel));
        if needed.is_none_or(|needed| buffer.len() < needed) {
            return Err(ConsoleError::BufferTooSmall);
        }

        let mut console = Console {
            buffer,
            text: None,
            info,
            columns,
            rows,
            row: 0,
            column: 0,
            foreground: Color::GREEN,
            background: Color::BLACK,
            escape: Escape::Text,
            cursor_visible: true,
            scale,
            font,
        };
        console.clear();
        Ok(console)
    }

    /// Switches to RAM text cells after heap initialization, clearing early output.
    /// Allocation failure leaves the allocation-free early console usable.
    /// Call `flush` to present the new screen. Repeated calls are harmless.
    pub fn enable_buffering(&mut self) -> Result<(), TryReserveError> {
        if self.text.is_some() {
            return Ok(());
        }
        // One extra cell per row records the color of the right pixel margin.
        let count = (self.columns + 1) * self.rows;
        let mut cells = Vec::new();
        let mut displayed = Vec::new();
        let mut history = Vec::new();
        let mut saved_cells = Vec::new();
        let history_count = (self.columns + 1).saturating_mul(SCROLLBACK_LINES);
        cells.try_reserve_exact(count)?;
        displayed.try_reserve_exact(count)?;
        history.try_reserve_exact(history_count)?;
        saved_cells.try_reserve_exact(count)?;
        cells.resize(count, Cell::blank(self.background));
        displayed.resize(count, None);
        history.resize(history_count, Cell::blank(self.background));
        saved_cells.resize(count, Cell::blank(self.background));
        self.text = Some(TextBuffer {
            cells, displayed, bottom_color: self.background, bottom_dirty: true,
            history, history_head: 0, history_len: 0, view_offset: 0,
            saved_cells, saved_state: None,
        });
        self.clear();
        Ok(())
    }

    pub fn is_buffered(&self) -> bool {
        self.text.is_some()
    }

    pub fn history_lines(&self) -> usize {
        self.text.as_ref().map_or(0, |text| text.history_len)
    }

    pub fn view_offset(&self) -> usize {
        self.text.as_ref().map_or(0, |text| text.view_offset)
    }

    /// Positive pages look into history, negative pages approach the live screen.
    /// New output keeps a historical viewport anchored until its oldest row expires.
    pub fn scroll_pages(&mut self, pages: isize) {
        let step = self.rows.saturating_sub(1).max(1);
        if let Some(text) = self.text.as_mut() {
            if text.saved_state.is_some() { return; }
            let lines = pages.unsigned_abs().saturating_mul(step);
            text.view_offset = if pages >= 0 {
                text.view_offset.saturating_add(lines).min(text.history_len)
            } else {
                text.view_offset.saturating_sub(lines)
            };
        }
    }

    pub fn return_live(&mut self) {
        if let Some(text) = self.text.as_mut() {
            text.view_offset = 0;
        }
    }

    pub fn alternate_screen_active(&self) -> bool {
        self.text.as_ref().is_some_and(|text| text.saved_state.is_some())
    }

    /// DEC private mode 1049: preserve the shell screen and exclude game frames
    /// from scrollback. Storage is reserved at initialization, never on entry.
    pub fn set_alternate_screen(&mut self, enabled: bool) {
        let Some(text) = self.text.as_mut() else { return };
        text.view_offset = 0;
        if enabled && text.saved_state.is_none() {
            text.saved_cells.copy_from_slice(&text.cells);
            text.saved_state = Some(ScreenState {
                row: self.row, column: self.column, foreground: self.foreground,
                background: self.background, cursor_visible: self.cursor_visible,
                bottom_color: text.bottom_color,
            });
            text.cells.fill(Cell::blank(self.background));
            text.bottom_color = self.background;
            text.bottom_dirty = true;
            self.row = 0;
            self.column = 0;
        } else if !enabled && let Some(saved) = text.saved_state.take() {
            text.cells.copy_from_slice(&text.saved_cells);
            self.row = saved.row;
            self.column = saved.column;
            self.foreground = saved.foreground;
            self.background = saved.background;
            self.cursor_visible = saved.cursor_visible;
            text.bottom_color = saved.bottom_color;
            text.bottom_dirty = true;
        }
    }

    /// Presents one text row using only RAM reads and framebuffer writes.
    /// Returns the number of changed cells (including the right margin).
    /// The caller can release its IRQ lock between rows.
    pub fn flush_row(&mut self, row: usize) -> usize {
        if self.text.is_none() || row >= self.rows {
            return 0;
        }
        let mut changed = 0;
        for column in 0..=self.columns {
            let index = row * (self.columns + 1) + column;
            let text = self.text.as_ref().unwrap();
            let source_row = text.history_len - text.view_offset + row;
            let cell = if source_row < text.history_len {
                let history_row = (text.history_head + SCROLLBACK_LINES - text.history_len + source_row) % SCROLLBACK_LINES;
                text.history[history_row * (self.columns + 1) + column]
            } else {
                text.cells[(source_row - text.history_len) * (self.columns + 1) + column]
            };
            if text.displayed[index] == Some(cell) {
                continue;
            }
            if column == self.columns {
                let x = self.columns * self.cell_width();
                self.fill_rect(x, row * self.line_height(), self.info.width - x, self.line_height(), cell.background);
            } else {
                self.paint_cell(row, column, cell);
            }
            self.text.as_mut().unwrap().displayed[index] = Some(cell);
            changed += 1;
        }
        if row == self.rows - 1 {
            let text = self.text.as_ref().unwrap();
            if text.bottom_dirty {
                let color = text.bottom_color;
                let y = self.rows * self.line_height();
                self.fill_rect(0, y, self.info.width, self.info.height - y, color);
                self.text.as_mut().unwrap().bottom_dirty = false;
            }
        }
        changed
    }

    /// Presents all pending cells; the early console already draws immediately.
    pub fn flush(&mut self) -> usize {
        (0..self.rows).map(|row| self.flush_row(row)).sum()
    }

    pub fn columns(&self) -> usize {
        self.columns
    }

    pub fn scale(&self) -> usize {
        self.scale
    }

    pub fn cell_width(&self) -> usize {
        self.font.width() * self.scale
    }

    pub fn line_height(&self) -> usize {
        self.font.height() * self.scale
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Text position `(row, column)` where the next character goes
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.column)
    }

    pub fn info(&self) -> FrameBufferInfo {
        self.info
    }

    pub fn colors(&self) -> (Color, Color) {
        (self.foreground, self.background)
    }

    pub fn set_colors(&mut self, foreground: Color, background: Color) {
        self.foreground = foreground;
        self.background = background;
    }

    pub fn write_string(&mut self, s: &str) {
        for c in s.chars() {
            self.write_char(c);
        }
    }

    pub fn write_char(&mut self, c: char) {
        if self.escape_char(c) {
            return;
        }
        match c {
            '\n' => self.new_line(),
            '\r' => self.column = 0,
            '\t' => {
                if self.column >= self.columns {
                    self.new_line();
                }
                // a tab never wraps: it stops at the next tab stop or at the line end
                let stop = (self.column / TAB_WIDTH + 1) * TAB_WIDTH;
                for column in self.column..stop.min(self.columns) {
                    self.clear_cell(self.row, column);
                }
                self.column = stop.min(self.columns);
            }
            // backspace: step back and erase, like a terminal echoing it
            '\u{8}' => {
                if self.column > 0 {
                    self.column = self.column.min(self.columns) - 1;
                    self.clear_cell(self.row, self.column);
                }
            }
            c if c.is_control() => {}
            c => {
                if self.column >= self.columns {
                    self.new_line();
                }
                if c == ' ' {
                    self.clear_cell(self.row, self.column);
                } else {
                    self.put_cell(self.row, self.column, Cell {
                        character: c, foreground: self.foreground, background: self.background,
                    });
                }
                self.column += 1;
            }
        }
    }

    pub fn clear(&mut self) {
        self.return_live();
        self.escape = Escape::Text;
        if let Some(text) = self.text.as_mut() {
            text.cells.fill(Cell::blank(self.background));
            text.bottom_color = self.background;
            text.bottom_dirty = true;
        } else {
            self.fill_rect(0, 0, self.info.width, self.info.height, self.background);
        }
        self.row = 0;
        self.column = 0;
    }

    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    /// A bounded streaming parser: sequences can span writes; malformed parameters never grow.
    fn escape_char(&mut self, c: char) -> bool {
        if c == '\x1b' {
            self.escape = Escape::Start;
            return true;
        }
        match self.escape {
            Escape::Text => false,
            Escape::Start => {
                self.escape = if c == '[' {
                    Escape::Csi {
                        params: [0; 8],
                        index: 0,
                        private: false,
                        invalid: false,
                    }
                } else {
                    Escape::Text
                };
                c == '[' || !c.is_control()
            }
            Escape::Csi {
                mut params,
                mut index,
                mut private,
                mut invalid,
            } => {
                match c {
                    '0'..='9' => {
                        params[index] = params[index]
                            .saturating_mul(10)
                            .saturating_add(c as u16 - '0' as u16)
                    }
                    ';' if index < params.len() - 1 => index += 1,
                    '?' if index == 0 && params[0] == 0 => private = true,
                    '\x40'..='\x7e' => {
                        self.escape = Escape::Text;
                        if !invalid {
                            self.csi(c, &params[..=index], private);
                        }
                        return true;
                    }
                    _ => invalid = true,
                }
                self.escape = Escape::Csi {
                    params,
                    index,
                    private,
                    invalid,
                };
                true
            }
        }
    }

    fn csi(&mut self, command: char, params: &[u16], private: bool) {
        let amount = (params[0] as usize).max(1);
        if private {
            if params == [1049] && matches!(command, 'h' | 'l') {
                self.set_alternate_screen(command == 'h');
            }
            if params == [25] && matches!(command, 'h' | 'l') {
                self.cursor_visible = command == 'h';
            }
            return;
        }
        match command {
            'A' => self.row = self.row.saturating_sub(amount),
            'B' => self.row = self.row.saturating_add(amount).min(self.rows - 1),
            'C' => self.column = self.column.saturating_add(amount).min(self.columns - 1),
            'D' => self.column = self.column.saturating_sub(amount),
            'G' => self.column = (amount - 1).min(self.columns - 1),
            'H' | 'f' => {
                self.row = (amount - 1).min(self.rows - 1);
                self.column = (params.get(1).copied().unwrap_or(1) as usize)
                    .saturating_sub(1)
                    .min(self.columns - 1);
            }
            'J' | 'K' => {
                if command == 'J' && params[0] == 3 && let Some(text) = self.text.as_mut() {
                    if text.saved_state.is_some() { return; }
                    text.history_head = 0;
                    text.history_len = 0;
                    text.view_offset = 0;
                    return;
                }
                let cursor = self.row * self.columns + self.column.min(self.columns - 1);
                let (start, end) = if command == 'J' {
                    (0, self.rows * self.columns)
                } else {
                    (self.row * self.columns, (self.row + 1) * self.columns)
                };
                let range = match params[0] {
                    0 => cursor..end,
                    1 => start..cursor + 1,
                    2 | 3 if command == 'J' => start..end,
                    2 => start..end,
                    _ => return,
                };
                // Clear whole row spans, not one small rectangle per character.
                let mut cell = range.start;
                while cell < range.end {
                    let row = cell / self.columns;
                    let end = range.end.min((row + 1) * self.columns);
                    if let Some(text) = self.text.as_mut() {
                        let start = row * (self.columns + 1) + cell % self.columns;
                        text.cells[start..start + end - cell].fill(Cell::blank(self.background));
                    } else {
                        self.fill_rect(
                            (cell % self.columns) * self.cell_width(), row * self.line_height(),
                            (end - cell) * self.cell_width(), self.line_height(), self.background,
                        );
                    }
                    cell = end;
                }
            }
            'm' => {
                for &param in params {
                    match param {
                        0 => self.set_colors(Color::GREEN, Color::BLACK),
                        30..=37 => self.foreground = ansi_color(param - 30, false),
                        40..=47 => self.background = ansi_color(param - 40, false),
                        90..=97 => self.foreground = ansi_color(param - 90, true),
                        100..=107 => self.background = ansi_color(param - 100, true),
                        39 => self.foreground = Color::GREEN,
                        49 => self.background = Color::BLACK,
                        _ => {} // blink and other text attributes are accepted but not rendered
                    }
                }
            }
            _ => {}
        }
    }

    /// Color of the pixel at (`x`, `y`), `None` outside of the visible screen
    pub fn read_pixel(&self, x: usize, y: usize) -> Option<Color> {
        if x >= self.info.width || y >= self.info.height {
            return None;
        }
        let offset = self.pixel_offset(x, y);
        Some(decode(self.info.pixel_format, &self.buffer[offset..offset + self.info.bytes_per_pixel]))
    }

    /// Checks that the text cell at (`row`, `col`) shows exactly the glyph of `c`
    /// in the current colors
    pub fn cell_shows(&self, row: usize, col: usize, c: char) -> bool {
        self.cell_shows_in(row, col, c, self.foreground, self.background)
    }

    /// Checks that the text cell at (`row`, `col`) shows exactly the glyph of `c`
    /// drawn with `foreground` on `background`
    pub fn cell_shows_in(&self, row: usize, col: usize, c: char, foreground: Color, background: Color) -> bool {
        if row >= self.rows || col >= self.columns {
            return false;
        }
        let (x0, y0) = (col * self.cell_width(), row * self.line_height());
        let glyph = self.font.glyph(c);
        (0..self.line_height()).all(|dy| {
            (0..self.cell_width()).all(|dx| {
                let offset = self.pixel_offset(x0 + dx, y0 + dy);
                let color = if self.font.pixel(glyph[dy / self.scale], dx / self.scale) { foreground } else { background };
                let (expected, len) = self.encode(color);
                self.buffer[offset..offset + len] == expected[..len]
            })
        })
    }

    fn new_line(&mut self) {
        self.column = 0;
        if self.row + 1 < self.rows {
            self.row += 1;
            return;
        }
        if let Some(text) = self.text.as_mut() {
            let columns = self.columns + 1;
            if text.saved_state.is_none() {
                let start = text.history_head * columns;
                text.history[start..start + columns].copy_from_slice(&text.cells[..columns]);
                text.history_head = (text.history_head + 1) % SCROLLBACK_LINES;
                text.history_len = (text.history_len + 1).min(SCROLLBACK_LINES);
                if text.view_offset != 0 {
                    text.view_offset = (text.view_offset + 1).min(text.history_len);
                }
            }
            text.cells.copy_within(columns.., 0);
            text.cells[(self.rows - 1) * columns..].fill(Cell::blank(self.background));
            return;
        }
        // Early boot only: the heap and RAM text cells do not exist yet.
        let line_bytes = self.line_height() * self.info.stride * self.info.bytes_per_pixel;
        let text_bytes = self.rows * line_bytes;
        self.buffer.copy_within(line_bytes..text_bytes, 0);
        self.fill_rect(0, (self.rows - 1) * self.line_height(), self.info.width, self.line_height(), self.background);
    }

    fn clear_cell(&mut self, row: usize, col: usize) {
        self.put_cell(row, col, Cell::blank(self.background));
    }

    fn put_cell(&mut self, row: usize, column: usize, cell: Cell) {
        if let Some(text) = self.text.as_mut() {
            text.cells[row * (self.columns + 1) + column] = cell;
        } else {
            self.paint_cell(row, column, cell);
        }
    }

    fn paint_cell(&mut self, row: usize, column: usize, cell: Cell) {
        if cell.character == ' ' {
            self.fill_rect(column * self.cell_width(), row * self.line_height(), self.cell_width(), self.line_height(), cell.background);
        } else {
            let colors = self.colors();
            self.set_colors(cell.foreground, cell.background);
            self.draw_glyph(row, column, cell.character);
            self.set_colors(colors.0, colors.1);
        }
    }

    fn fill_rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: Color) {
        let (bytes, len) = self.encode(color);
        let bytes_per_pixel = self.info.bytes_per_pixel;
        let uniform = bytes[..len].iter().all(|&byte| byte == bytes[0]);
        for y in y..(y + height).min(self.info.height) {
            let start = self.pixel_offset(x, y);
            let end = self.pixel_offset((x + width).min(self.info.width), y);
            let line = &mut self.buffer[start..end];
            if uniform {
                line.fill(bytes[0]);
            } else {
                for pixel in line.chunks_exact_mut(bytes_per_pixel) {
                    pixel[..len].copy_from_slice(&bytes[..len]);
                }
            }
        }
    }

    fn draw_glyph(&mut self, row: usize, col: usize, c: char) {
        let (x0, y0) = (col * self.cell_width(), row * self.line_height());
        let raster = self.font.glyph(c);
        let (foreground, bpp) = self.encode(self.foreground);
        let (background, _) = self.encode(self.background);
        let line_bytes = self.cell_width() * bpp;
        // Largest native raster, maximum supported scale and pixel size. Compose
        // a scanline in RAM, then issue a contiguous write to video memory.
        let mut line = [0; Font::Large.width() * 3 * 4];
        for (dy, &bits) in raster.iter().enumerate() {
            for (dx, pixel) in line[..line_bytes].chunks_exact_mut(bpp).enumerate() {
                let bytes = if self.font.pixel(bits, dx / self.scale) { foreground } else { background };
                pixel.copy_from_slice(&bytes[..bpp]);
            }
            for sy in 0..self.scale {
                let offset = self.pixel_offset(x0, y0 + dy * self.scale + sy);
                self.buffer[offset..offset + line_bytes].copy_from_slice(&line[..line_bytes]);
            }
        }
    }

    fn pixel_offset(&self, x: usize, y: usize) -> usize {
        (y * self.info.stride + x) * self.info.bytes_per_pixel
    }

    /// Converts a color into the framebuffer pixel bytes and their count
    fn encode(&self, color: Color) -> ([u8; 4], usize) {
        (encode(self.info.pixel_format, color), self.info.bytes_per_pixel)
    }
}

/// Pixel bytes of `color`, unused trailing bytes are zero
pub fn encode(format: PixelFormat, color: Color) -> [u8; 4] {
    match format {
        PixelFormat::Rgb => [color.r, color.g, color.b, 0],
        PixelFormat::Bgr => [color.b, color.g, color.r, 0],
        _ => [gray(color), 0, 0, 0],
    }
}

/// Color of the pixel `bytes`, grayscale for one byte formats
pub fn decode(format: PixelFormat, bytes: &[u8]) -> Color {
    match format {
        PixelFormat::Rgb => Color::new(bytes[0], bytes[1], bytes[2]),
        PixelFormat::Bgr => Color::new(bytes[2], bytes[1], bytes[0]),
        _ => Color::new(bytes[0], bytes[0], bytes[0]),
    }
}

fn gray(color: Color) -> u8 {
    ((color.r as u16 + color.g as u16 + color.b as u16) / 3) as u8
}

fn ansi_color(index: u16, bright: bool) -> Color {
    let low = if bright { 85 } else { 0 };
    let high = if bright { 255 } else { 170 };
    Color::new(
        if index & 1 != 0 { high } else { low },
        if index & 2 != 0 { high } else { low },
        if index & 4 != 0 { high } else { low },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::fmt::Write;

    /// Extra bytes after the framebuffer to catch writes past its end
    const GUARD: usize = 64;
    const SENTINEL: u8 = 0xab;

    fn info(format: PixelFormat, bytes_per_pixel: usize, width: usize, height: usize, stride: usize) -> FrameBufferInfo {
        FrameBufferInfo {
            byte_len: stride * height * bytes_per_pixel,
            width,
            height,
            pixel_format: format,
            bytes_per_pixel,
            stride,
        }
    }

    /// A 10x4 cells screen with a little margin to the right and below
    fn screen_info() -> FrameBufferInfo {
        info(PixelFormat::Bgr, 4, CHAR_WIDTH * 10 + 3, LINE_HEIGHT * 4 + 5, CHAR_WIDTH * 10 + 8)
    }

    fn buffer(info: &FrameBufferInfo) -> Vec<u8> {
        vec![SENTINEL; info.byte_len + GUARD]
    }

    fn console(buffer: &mut [u8], info: FrameBufferInfo) -> Console<'_> {
        Console::new(buffer, info).expect("valid framebuffer")
    }

    fn assert_guard_untouched(buffer: &[u8], info: &FrameBufferInfo) {
        assert!(buffer[info.byte_len..].iter().all(|&b| b == SENTINEL), "write past the framebuffer end");
    }

    fn row_text_is(console: &Console, row: usize, text: &str) -> bool {
        text.chars().enumerate().all(|(col, c)| console.cell_shows(row, col, c))
    }

    #[test]
    fn scrollback_preserves_colors_and_viewport_while_output_continues() {
        let info = screen_info();
        let mut bytes = buffer(&info);
        let mut c = console(&mut bytes, info);
        c.enable_buffering().unwrap();
        c.write_string("\x1b[31;44ma\n\x1b[0mb\nc\nd\n");
        assert_eq!(c.history_lines(), 1);
        let cursor = c.cursor();
        c.scroll_pages(1);
        c.flush();
        assert_eq!(c.view_offset(), 1);
        assert_eq!(c.cursor(), cursor);
        assert!(c.cell_shows_in(0, 0, 'a', Color::RED, Color::new(0, 0, 170)));
        assert!(c.cell_shows(3, 0, 'd'));
        c.write_string("e\n");
        assert_eq!(c.view_offset(), 2);
        assert_eq!(c.flush(), 0, "new output must not replace the historical viewport");
        c.scroll_pages(-1);
        c.flush();
        assert_eq!(c.view_offset(), 0);
        assert!(c.cell_shows(0, 0, 'c'));
        assert!(c.cell_shows(2, 0, 'e'));
        assert_eq!(c.cursor(), (3, 0));
    }

    #[test]
    fn scrollback_wraps_at_capacity_and_page_navigation_clamps() {
        let info = screen_info();
        let mut bytes = buffer(&info);
        let mut c = console(&mut bytes, info);
        c.enable_buffering().unwrap();
        let lines = SCROLLBACK_LINES + 8;
        for i in 0..lines { writeln!(c, "{}", (b'a' + (i % 26) as u8) as char).unwrap(); }
        assert_eq!(c.history_lines(), SCROLLBACK_LINES);
        c.scroll_pages(isize::MAX);
        c.flush();
        let oldest = lines - (c.rows() - 1) - SCROLLBACK_LINES;
        assert_eq!(c.view_offset(), SCROLLBACK_LINES);
        assert!(c.cell_shows(0, 0, (b'a' + (oldest % 26) as u8) as char));
        c.write_string("z\n");
        c.flush();
        assert!(c.cell_shows(0, 0, (b'a' + ((oldest + 1) % 26) as u8) as char));
        c.scroll_pages(isize::MIN);
        assert_eq!(c.view_offset(), 0);
        c.scroll_pages(1);
        c.return_live();
        c.flush();
        assert_eq!(c.view_offset(), 0);
        assert!(c.cell_shows(2, 0, 'z'));
        assert_guard_untouched(&bytes, &info);
    }

    #[test]
    fn alternate_screen_restores_shell_and_does_not_record_game_frames() {
        let info = screen_info();
        let mut bytes = buffer(&info);
        let mut c = console(&mut bytes, info);
        c.enable_buffering().unwrap();
        c.write_string("one\ntwo\nthree\nfour\nfive\x1b[31m");
        c.flush();
        let saved = c.buffer.to_vec();
        let cursor = c.cursor();
        let colors = c.colors();
        let history = c.history_lines();
        c.write_string("\x1b[?1049h\x1b[?1049h");
        for _ in 0..SCROLLBACK_LINES + 5 { c.write_string("game\n"); }
        c.scroll_pages(5);
        c.write_string("\x1b[3J");
        c.flush();
        assert_eq!(c.history_lines(), history);
        assert_eq!(c.view_offset(), 0);
        c.write_string("\x1b[?1049l\x1b[?1049l");
        c.flush();
        assert_eq!(c.buffer, saved);
        assert_eq!(c.cursor(), cursor);
        assert_eq!(c.colors(), colors);
        c.scroll_pages(1);
        c.flush();
        assert!(c.cell_shows_in(0, 0, 'o', Color::GREEN, Color::BLACK));
        c.write_string("\x1b[3J");
        c.flush();
        assert_eq!(c.history_lines(), 0);
        assert_eq!(c.view_offset(), 0);
        assert!(c.cell_shows_in(0, 0, 't', Color::GREEN, Color::BLACK));
    }

    #[test]
    fn buffered_output_matches_direct_pixels_across_formats_fonts_and_ansi() {
        for (format, bpp) in [(PixelFormat::Rgb, 3), (PixelFormat::Rgb, 4),
            (PixelFormat::Bgr, 3), (PixelFormat::Bgr, 4),
            (PixelFormat::U8, 1), (PixelFormat::U8, 2), (PixelFormat::U8, 3), (PixelFormat::U8, 4)] {
            for (font, scale) in [(Font::Small, 1), (Font::Small, 2), (Font::Small, 3), (Font::Large, 1)] {
                let width = font.width() * scale * 10 + 3;
                let height = font.height() * scale * 4 + 5;
                let info = info(format, bpp, width, height, width + 7);
                let mut a = buffer(&info);
                let mut b = buffer(&info);
                let mut direct = Console::with_font_and_scale(&mut a, info, font, scale).unwrap();
                let mut buffered = Console::with_font_and_scale(&mut b, info, font, scale).unwrap();
                buffered.enable_buffering().unwrap();
                buffered.flush();
                for _ in 0..3 {
                    for text in ["one\ntwo\n", "\x1b[31;44mЖ\t!\n", "wrap over several rows of text\n\n",
                        "\x1b[2;3H\x1b[", "1Kabc\x08Z", "\x1b[2J", "\x1b[3;1H\x1b[42m\n\n",
                        "\x1b[0m\x1b[H0123456789\t!\rQ", "\x1b[1J\x1b[2K\x1b[?25l"] {
                        direct.write_string(text);
                        buffered.write_string(text);
                        buffered.flush();
                        assert_eq!(direct.cursor(), buffered.cursor());
                        assert_eq!(direct.colors(), buffered.colors());
                        assert_eq!(direct.cursor_visible(), buffered.cursor_visible());
                        assert_eq!(direct.buffer, buffered.buffer, "format={format:?} bpp={bpp} scale={scale} text={text:?}");
                        assert_eq!(buffered.flush(), 0, "unchanged screen must require no writes");
                    }
                    direct.set_colors(Color::WHITE, Color::RED);
                    buffered.set_colors(Color::WHITE, Color::RED);
                    direct.clear();
                    buffered.clear();
                    buffered.flush();
                    assert_eq!(direct.buffer, buffered.buffer);
                }
                drop(direct);
                drop(buffered);
                assert_guard_untouched(&a, &info);
                assert_guard_untouched(&b, &info);
            }
        }
    }

    #[test]
    fn buffered_scroll_reconstructs_pixels_without_reading_framebuffer() {
        let info = screen_info();
        let mut bytes = buffer(&info);
        let mut c = console(&mut bytes, info);
        c.enable_buffering().unwrap();
        c.write_string("a\n\x1b[31;44mb\n\x1b[0mc\nd");
        c.flush();
        // Simulate unreadable/meaningless video memory. Reconstruct the scrolled
        // screen from text alone, including each glyph's original colors.
        c.buffer[..info.byte_len].fill(0xdd);
        c.text.as_mut().unwrap().displayed.fill(None);
        c.write_char('\n');
        c.flush();
        assert!(c.cell_shows_in(0, 0, 'b', Color::RED, Color::new(0, 0, 170)));
        assert!(c.cell_shows_in(1, 0, 'c', Color::GREEN, Color::BLACK));
        assert!(c.cell_shows_in(2, 0, 'd', Color::GREEN, Color::BLACK));
        assert!(c.cell_shows(3, 0, ' '));
        assert_guard_untouched(&bytes, &info);
    }

    #[test]
    fn buffered_scroll_skips_identical_cells_and_coalesces_intermediate_screens() {
        let info = screen_info();
        let mut bytes = buffer(&info);
        let mut c = console(&mut bytes, info);
        c.enable_buffering().unwrap();
        c.flush();
        c.write_string("same\nsame\nsame\n");
        c.flush();
        c.write_string("same\n");
        assert_eq!(c.flush(), 0, "identical scrolled rows need no video writes");
        for _ in 0..1000 {
            c.write_string("transient\n");
        }
        c.write_string("\x1b[H\x1b[2Jsame\nsame\nsame\n");
        assert_eq!(c.flush(), 0, "intermediate screens are never drawn");
        c.write_string("\x1b[H\x1b[31mS");
        assert_eq!(c.flush_row(0), 1);
        assert_eq!(c.flush(), 0);
    }

    #[test]
    fn native_fonts_render_clear_scroll_and_preserve_padding() {
        for font in [Font::Small, Font::Large] {
            for (format, bpp) in [(PixelFormat::Rgb, 3), (PixelFormat::Bgr, 4), (PixelFormat::U8, 1)] {
                let info = info(format, bpp, font.width() * 4 + 3, font.height() * 2 + 5, font.width() * 4 + 8);
                let mut bytes = buffer(&info);
                let mut console = Console::new_with_font(&mut bytes, info, font).unwrap();
                assert_eq!((console.columns(), console.rows()), (4, 2));
                console.set_colors(Color::WHITE, Color::RED);
                console.clear();
                console.write_string("WЖg?");
                assert!(row_text_is(&console, 0, "WЖg?"));
                console.write_string("\r \x1b[K");
                assert!(row_text_is(&console, 0, "    "));
                console.write_string("\x1b[2;1HЖ😀A\n");
                assert!(row_text_is(&console, 0, "Ж\u{fffd}A "));
                assert!(row_text_is(&console, 1, "    "));
                console.write_string("\x1b[0m\x1b[2J\x1b[H#");
                let raster = font.glyph('#');
                for y in 0..font.height() {
                    for x in 0..font.width() {
                        let on = font.pixel(raster[y], x);
                        let expected = if on { Color::GREEN } else { Color::BLACK };
                        let encoded = encode(format, expected);
                        assert_eq!(console.read_pixel(x, y), Some(decode(format, &encoded[..bpp])));
                    }
                }
                assert_guard_untouched(&bytes, &info);
                for y in 0..info.height {
                    let padding = (y * info.stride + info.width) * bpp..(y + 1) * info.stride * bpp;
                    assert!(bytes[padding].iter().all(|&byte| byte == SENTINEL));
                }
            }
        }
    }

    #[test]
    fn scaled_console_matches_every_unscaled_pixel() {
        for (format, bpp) in [(PixelFormat::Rgb, 3), (PixelFormat::Bgr, 4), (PixelFormat::U8, 1)] {
            for scale in [2, 3] {
                let small = info(format, bpp, CHAR_WIDTH * 10 + 3, LINE_HEIGHT * 4 + 5, CHAR_WIDTH * 10 + 8);
                let large = info(format, bpp, small.width * scale, small.height * scale, small.stride * scale);
                let mut a = buffer(&small);
                let mut b = buffer(&large);
                let mut original = Console::new(&mut a, small).unwrap();
                let mut scaled = Console::new_scaled(&mut b, large, scale).unwrap();
                assert_eq!((scaled.columns(), scaled.rows()), (original.columns(), original.rows()));
                assert_eq!(scaled.scale(), scale);
                for text in ["Ж# W?", "\rA\tZ\x08", "\x1b[2;3H\x1b[31;47m#Ж ", "\x1b[1K", "\x1b[0m\n1\n2\n3\n4\n", "\x1b[2;2H\x1b[J", "\x1b[2J"] {
                    original.write_string(text);
                    scaled.write_string(text);
                    assert_eq!(scaled.cursor(), original.cursor());
                    for y in 0..large.height {
                        for x in 0..large.width {
                            assert_eq!(scaled.read_pixel(x, y), original.read_pixel(x / scale, y / scale),
                                "scale {scale}, pixel {x},{y}, after {text:?}");
                        }
                    }
                }
                assert_guard_untouched(&b, &large);
                for y in 0..large.height {
                    let padding = (y * large.stride + large.width) * bpp..(y + 1) * large.stride * bpp;
                    assert!(b[padding].iter().all(|&byte| byte == SENTINEL));
                }
            }
        }
    }

    #[test]
    fn scaled_console_rejects_bad_scale_and_too_small_screen() {
        let info = screen_info();
        let mut bytes = buffer(&info);
        for scale in [0, 4, usize::MAX] {
            assert!(matches!(Console::new_scaled(&mut bytes, info, scale), Err(ConsoleError::UnsupportedScale)));
        }
        let tiny = self::info(PixelFormat::Bgr, 4, CHAR_WIDTH, LINE_HEIGHT, CHAR_WIDTH);
        let mut bytes = buffer(&tiny);
        assert!(matches!(Console::new_scaled(&mut bytes, tiny, 2), Err(ConsoleError::ScreenTooSmall)));
    }

    #[test]
    fn blend_extremes() {
        let (bg, fg) = (Color::new(10, 20, 30), Color::new(200, 100, 50));
        assert_eq!(bg.blend(fg, 0), bg);
        assert_eq!(bg.blend(fg, 255), fg);
        let half = bg.blend(fg, 128);
        assert!(half.r > bg.r && half.r < fg.r);
    }

    #[test]
    fn encode_decode_round_trip() {
        let color = Color::new(1, 2, 3);
        assert_eq!(encode(PixelFormat::Rgb, color), [1, 2, 3, 0]);
        assert_eq!(encode(PixelFormat::Bgr, color), [3, 2, 1, 0]);
        assert_eq!(decode(PixelFormat::Rgb, &[1, 2, 3, 0]), color);
        assert_eq!(decode(PixelFormat::Bgr, &[3, 2, 1, 0]), color);
        assert_eq!(encode(PixelFormat::U8, Color::new(30, 60, 90)), [60, 0, 0, 0]);
        assert_eq!(decode(PixelFormat::U8, &[60]), Color::new(60, 60, 60));
    }

    #[test]
    fn rejects_unusable_framebuffers() {
        let unknown = PixelFormat::Unknown { red_position: 11, green_position: 5, blue_position: 0 };
        let cases = [
            (info(unknown, 2, 640, 480, 640), ConsoleError::UnsupportedPixelFormat(unknown)),
            (info(PixelFormat::Rgb, 2, 640, 480, 640), ConsoleError::UnsupportedPixelSize(2)),
            (info(PixelFormat::Rgb, 8, 640, 480, 640), ConsoleError::UnsupportedPixelSize(8)),
            (info(PixelFormat::U8, 0, 640, 480, 640), ConsoleError::UnsupportedPixelSize(0)),
            (info(PixelFormat::Rgb, 4, 640, 480, 600), ConsoleError::InvalidStride),
            (info(PixelFormat::Rgb, 4, CHAR_WIDTH - 1, 480, 640), ConsoleError::ScreenTooSmall),
            (info(PixelFormat::Rgb, 4, 640, LINE_HEIGHT - 1, 640), ConsoleError::ScreenTooSmall),
            (info(PixelFormat::Rgb, 4, 0, 0, 0), ConsoleError::ScreenTooSmall),
        ];
        for (info, error) in cases {
            let mut buffer = vec![0; 640 * 480 * 8];
            assert_eq!(Console::new(&mut buffer, info).err(), Some(error), "{:?}", info);
        }
    }

    #[test]
    fn rejects_short_buffer() {
        let info = screen_info();
        let mut buffer = vec![0; info.byte_len - 1];
        assert_eq!(Console::new(&mut buffer, info).err(), Some(ConsoleError::BufferTooSmall));
    }

    #[test]
    fn smallest_screen_is_one_cell() {
        let info = info(PixelFormat::Rgb, 3, CHAR_WIDTH, LINE_HEIGHT, CHAR_WIDTH);
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        assert_eq!((console.columns(), console.rows()), (1, 1));
        for c in "wrap and scroll a lot\n\n\n".chars() {
            console.write_char(c);
        }
        console.write_char('z');
        assert!(console.cell_shows(0, 0, 'z'));
        assert_guard_untouched(&buffer, &info);
    }

    #[test]
    fn geometry() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let console = console(&mut buffer, info);
        assert_eq!((console.columns(), console.rows()), (10, 4));
        assert_eq!(console.cursor(), (0, 0));
    }

    #[test]
    fn clear_paints_visible_pixels_only() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let console = console(&mut buffer, info);
        for y in 0..info.height {
            for x in 0..info.width {
                assert_eq!(console.read_pixel(x, y), Some(Color::BLACK));
            }
        }
        drop(console);
        // padding between width and stride keeps whatever was there
        let padding = (info.width * info.bytes_per_pixel)..(info.stride * info.bytes_per_pixel);
        assert!(buffer[padding].iter().all(|&b| b == SENTINEL));
        assert_guard_untouched(&buffer, &info);
    }

    #[test]
    fn read_pixel_bounds() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let console = console(&mut buffer, info);
        assert!(console.read_pixel(info.width - 1, info.height - 1).is_some());
        assert_eq!(console.read_pixel(info.width, 0), None);
        assert_eq!(console.read_pixel(0, info.height), None);
        assert_eq!(console.read_pixel(usize::MAX, usize::MAX), None);
    }

    #[test]
    fn writes_text_at_cursor() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        write!(console, "abc").unwrap();
        assert!(row_text_is(&console, 0, "abc"));
        assert!(!console.cell_shows(0, 0, 'b'));
        assert_eq!(console.cursor(), (0, 3));
        // glyphs are antialiased: pixels are shades between background and foreground
        let cell = (0..LINE_HEIGHT).flat_map(|y| (0..CHAR_WIDTH).map(move |x| (x, y)));
        let pixels: Vec<Color> = cell.map(|(x, y)| console.read_pixel(x, y).unwrap()).collect();
        assert!(pixels.iter().any(|p| p.g > 0x80), "glyph is drawn");
        assert!(pixels.iter().all(|p| p.r == 0 && p.b == 0), "only shades of green on black");
    }

    #[test]
    fn newline_and_carriage_return() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        write!(console, "abc\ndef\rX").unwrap();
        assert!(row_text_is(&console, 0, "abc"));
        assert!(row_text_is(&console, 1, "Xef"));
        assert_eq!(console.cursor(), (1, 1));
    }

    #[test]
    fn wraps_long_lines() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        write!(console, "0123456789AB").unwrap();
        assert!(row_text_is(&console, 0, "0123456789"));
        assert!(row_text_is(&console, 1, "AB"));
    }

    #[test]
    fn full_line_then_newline_has_no_blank_line() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        write!(console, "0123456789\nx").unwrap();
        assert!(row_text_is(&console, 1, "x"));
    }

    #[test]
    fn scrolls_when_full() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        for line in ["l0", "l1", "l2", "l3", "l4", "l5"] {
            writeln!(console, "{}", line).unwrap();
        }
        // 4 rows: the last row is empty for the cursor, above it l3..l5
        assert!(row_text_is(&console, 0, "l3"));
        assert!(row_text_is(&console, 1, "l4"));
        assert!(row_text_is(&console, 2, "l5"));
        assert!(row_text_is(&console, 3, "  "));
        assert_eq!(console.cursor(), (3, 0));
        drop(console);
        assert_guard_untouched(&buffer, &info);
    }

    #[test]
    fn margins_stay_background_after_scrolling() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        for _ in 0..20 {
            writeln!(console, "##########").unwrap();
        }
        for y in console.rows() * LINE_HEIGHT..info.height {
            for x in 0..info.width {
                assert_eq!(console.read_pixel(x, y), Some(Color::BLACK), "bottom margin at {x},{y}");
            }
        }
        for y in 0..info.height {
            for x in console.columns() * CHAR_WIDTH..info.width {
                assert_eq!(console.read_pixel(x, y), Some(Color::BLACK), "right margin at {x},{y}");
            }
        }
    }

    #[test]
    fn tab_moves_to_next_stop() {
        let info = info(PixelFormat::Rgb, 3, CHAR_WIDTH * 20, LINE_HEIGHT * 2, CHAR_WIDTH * 20);
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        write!(console, "ab\tc\td").unwrap();
        assert!(console.cell_shows(0, 8, 'c'));
        assert!(console.cell_shows(0, 16, 'd'));
        assert!(console.cell_shows(0, 2, ' '));
        write!(console, "\t").unwrap();
        assert_eq!(console.cursor(), (0, 20), "the last tab stops at the line end");
    }

    #[test]
    fn tab_near_line_end_does_not_wrap() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        write!(console, "012345678\tX").unwrap();
        assert!(console.cell_shows(0, 9, ' '));
        assert!(console.cell_shows(1, 0, 'X'), "the char after the tab wraps normally");
    }

    #[test]
    fn tab_after_full_line_starts_on_the_next_line() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        write!(console, "0123456789\tX").unwrap();
        assert!(row_text_is(&console, 0, "0123456789"));
        assert!(console.cell_shows(1, 8, 'X'));
        assert_eq!(console.cursor(), (1, 9));
    }

    #[test]
    fn tab_on_a_one_cell_screen() {
        let info = info(PixelFormat::Rgb, 3, CHAR_WIDTH, LINE_HEIGHT, CHAR_WIDTH);
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        write!(console, "\t\t\tx").unwrap();
        assert!(console.cell_shows(0, 0, 'x'));
        drop(console);
        assert_guard_untouched(&buffer, &info);
    }

    #[test]
    fn carriage_return_after_full_line() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        write!(console, "0123456789\rX").unwrap();
        assert!(row_text_is(&console, 0, "X123456789"));
        assert_eq!(console.cursor(), (0, 1));
    }

    #[test]
    fn clear_moves_the_cursor_home() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        write!(console, "ab\ncd").unwrap();
        console.clear();
        assert_eq!(console.cursor(), (0, 0));
        write!(console, "x").unwrap();
        assert!(console.cell_shows(0, 0, 'x'));
    }

    #[test]
    fn scrolling_and_erasing_use_the_background_color() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        console.set_colors(Color::WHITE, Color::RED);
        console.clear();
        for _ in 0..20 {
            writeln!(console, "line").unwrap();
        }
        write!(console, "ab\u{8}").unwrap();
        let bottom = (console.rows() - 1) * LINE_HEIGHT;
        for y in bottom..bottom + LINE_HEIGHT {
            for x in CHAR_WIDTH..info.width {
                assert_eq!(console.read_pixel(x, y), Some(Color::RED), "{x},{y}");
            }
        }
    }

    #[test]
    fn cell_shows_with_other_colors() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        write!(console, "a").unwrap();
        console.set_colors(Color::WHITE, Color::RED);
        assert!(!console.cell_shows(0, 0, 'a'), "drawn in the old colors");
        assert!(console.cell_shows_in(0, 0, 'a', Color::GREEN, Color::BLACK));
    }

    #[test]
    fn cell_shows_checks_the_column() {
        let info = screen_info();
        let mut buffer = vec![0; info.byte_len];
        let console = console(&mut buffer, info);
        // the right margin is black like a blank cell, but it is no cell
        assert!(!console.cell_shows(0, console.columns(), ' '));
        assert!(console.cell_shows(0, console.columns() - 1, ' '));
    }

    #[test]
    fn scrolling_keeps_stride_padding() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        for _ in 0..20 {
            writeln!(console, "0123456789").unwrap();
        }
        drop(console);
        for y in 0..info.height {
            let row = y * info.stride * info.bytes_per_pixel;
            let padding = row + info.width * info.bytes_per_pixel..row + info.stride * info.bytes_per_pixel;
            assert!(buffer[padding].iter().all(|&b| b == SENTINEL), "row {y}");
        }
    }

    #[test]
    fn stride_must_cover_width() {
        let mut buffer = vec![0; 640 * 480 * 4];
        let narrow = info(PixelFormat::Rgb, 4, 640, 480, 639);
        assert_eq!(Console::new(&mut buffer, narrow).err(), Some(ConsoleError::InvalidStride));
        let exact = info(PixelFormat::Rgb, 4, 640, 480, 640);
        assert!(Console::new(&mut buffer, exact).is_ok());
    }

    #[test]
    fn gray_is_the_channel_average() {
        assert_eq!(encode(PixelFormat::U8, Color::new(30, 90, 0)), [40, 0, 0, 0]);
        assert_eq!(encode(PixelFormat::U8, Color::WHITE), [255, 0, 0, 0]);
    }

    #[test]
    fn backspace_erases_previous_char() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        write!(console, "ab\u{8}").unwrap();
        assert_eq!(console.cursor(), (0, 1));
        assert!(console.cell_shows(0, 1, ' '));
        write!(console, "c").unwrap();
        assert!(row_text_is(&console, 0, "ac"));
        // at the line start it does nothing
        write!(console, "\r\u{8}\u{8}").unwrap();
        assert_eq!(console.cursor(), (0, 0));
    }

    #[test]
    fn backspace_after_full_line() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        write!(console, "0123456789\u{8}").unwrap();
        assert_eq!(console.cursor(), (0, 9));
        assert!(console.cell_shows(0, 9, ' '));
    }

    #[test]
    fn control_chars_are_not_drawn() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        write!(console, "a\u{1b}\u{7f}\u{0}b").unwrap();
        assert!(row_text_is(&console, 0, "ab"));
    }

    #[test]
    fn unknown_chars_use_replacement_glyph() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        write!(console, "中").unwrap();
        assert!(console.cell_shows(0, 0, UNKNOWN_CHAR));
        assert!(
            console.cell_shows(0, 0, '中'),
            "cell_shows uses the same fallback"
        );
    }

    #[test]
    fn colors_apply_to_new_text() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        console.set_colors(Color::WHITE, Color::RED);
        assert_eq!(console.colors(), (Color::WHITE, Color::RED));
        console.clear();
        assert_eq!(console.read_pixel(0, 0), Some(Color::RED));
        write!(console, "x").unwrap();
        assert!(console.cell_shows(0, 0, 'x'));
    }

    #[test]
    fn ansi_sequences_span_writes_and_position_without_drawing_codes() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        write!(console, "ab\x1b[").unwrap();
        write!(console, "3;4HЖ\x1b[2A\x1b[2DX").unwrap();
        assert!(console.cell_shows(2, 3, 'Ж'));
        assert!(!console.cell_shows(2, 3, UNKNOWN_CHAR));
        assert!(console.cell_shows(0, 2, 'X'));
        write!(console, "\x1b[999999999999B\x1b[99999C").unwrap();
        assert_eq!(console.cursor(), (3, 9));
        write!(console, "\x1b[H\x1b[2J").unwrap();
        assert_eq!(console.cursor(), (0, 0));
        assert!(console.cell_shows(2, 3, ' '));
        assert_guard_untouched(&buffer, &info);
    }

    #[test]
    fn ansi_erase_preserves_cursor_and_sgr_restores_colors() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        write!(console, "abcdef\x1b[1;3H\x1b[K").unwrap();
        assert!(row_text_is(&console, 0, "ab    "));
        assert_eq!(console.cursor(), (0, 2));
        write!(console, "\x1b[30;5;107mX\x1b[0m").unwrap();
        assert!(console.cell_shows_in(0, 2, 'X', Color::BLACK, Color::WHITE));
        assert_eq!(console.colors(), (Color::GREEN, Color::BLACK));
        write!(console, "\x1b[?25l").unwrap();
        assert!(!console.cursor_visible());
        write!(console, "\x1b[?25h").unwrap();
        assert!(console.cursor_visible());
        write!(console, "\x1b[2J").unwrap();
        assert_eq!(console.cursor(), (0, 3));
    }

    #[test]
    fn optimized_drawing_overwrites_glyphs_and_spacing_in_all_formats() {
        for (format, bpp) in [(PixelFormat::Rgb, 3), (PixelFormat::Bgr, 4), (PixelFormat::U8, 1)] {
            let info = info(format, bpp, CHAR_WIDTH * 4, LINE_HEIGHT * 2, CHAR_WIDTH * 4 + 5);
            let mut buffer = buffer(&info);
            let mut console = console(&mut buffer, info);
            console.set_colors(Color::WHITE, Color::RED);
            console.write_string("ЖW#X");
            console.set_colors(Color::GREEN, Color::BLACK);
            console.write_string("\r #Ж ");
            assert!(row_text_is(&console, 0, " #Ж "));
            for x in 0..info.width {
                assert_eq!(console.read_pixel(x, LINE_HEIGHT - 1), Some(Color::BLACK));
            }
            console.write_string("\x1b[1;2H\x1b[J");
            assert!(row_text_is(&console, 0, "    "));
            assert!(row_text_is(&console, 1, "    "));
            assert_guard_untouched(&buffer, &info);
            for row in 0..info.height {
                let padding = (row * info.stride + info.width) * bpp..(row + 1) * info.stride * bpp;
                assert!(buffer[padding].iter().all(|&b| b == SENTINEL));
            }
        }
    }

    #[test]
    fn malformed_ansi_is_bounded_and_recovers_at_final_byte() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let mut console = console(&mut buffer, info);
        write!(console, "\x1b[1;2;3;4;5;6;7;8;9;10HQ").unwrap();
        assert_eq!(console.cursor(), (0, 1));
        assert!(console.cell_shows(0, 0, 'Q'));
        write!(console, "\x1b[\x1b[2;2HZ").unwrap();
        assert!(console.cell_shows(1, 1, 'Z'));
        assert_guard_untouched(&buffer, &info);
    }

    #[test]
    fn every_pixel_format() {
        for (format, bytes_per_pixel) in [
            (PixelFormat::Rgb, 3),
            (PixelFormat::Rgb, 4),
            (PixelFormat::Bgr, 3),
            (PixelFormat::Bgr, 4),
            (PixelFormat::U8, 1),
            (PixelFormat::U8, 4),
        ] {
            let info = info(format, bytes_per_pixel, CHAR_WIDTH * 3, LINE_HEIGHT * 2, CHAR_WIDTH * 3 + 1);
            let mut buffer = buffer(&info);
            let mut console = console(&mut buffer, info);
            writeln!(console, "abcdef\nghi").unwrap();
            assert!(row_text_is(&console, 0, "ghi"), "{:?} {}", format, bytes_per_pixel);
            drop(console);
            assert_guard_untouched(&buffer, &info);
        }
    }

    #[test]
    fn cell_shows_out_of_range() {
        let info = screen_info();
        let mut buffer = buffer(&info);
        let console = console(&mut buffer, info);
        assert!(!console.cell_shows(4, 0, ' '));
        assert!(!console.cell_shows(0, 10, ' '));
    }
}
