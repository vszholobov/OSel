//! Text console drawn into a linear framebuffer
//!
//! Glyphs come from the Noto Sans Mono bitmap font. Text goes from the top down,
//! once the cursor reaches the bottom row the screen scrolls up on every new line.

use bootloader_api::info::{FrameBufferInfo, PixelFormat};
use core::fmt;
use noto_sans_mono_bitmap::{FontWeight, RasterHeight, RasterizedChar, get_raster, get_raster_width};

const FONT_WEIGHT: FontWeight = FontWeight::Regular;
const CHAR_RASTER_HEIGHT: RasterHeight = RasterHeight::Size16;
/// Width of a text cell in pixels
pub const CHAR_WIDTH: usize = get_raster_width(FONT_WEIGHT, CHAR_RASTER_HEIGHT);
/// Height of a text cell in pixels: the glyph plus line spacing
pub const LINE_HEIGHT: usize = CHAR_RASTER_HEIGHT.val() + 2;
/// Tab stops are every `TAB_WIDTH` columns
pub const TAB_WIDTH: usize = 8;
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

pub struct Console<'a> {
    buffer: &'a mut [u8],
    info: FrameBufferInfo,
    columns: usize,
    rows: usize,
    row: usize,
    column: usize,
    foreground: Color,
    background: Color,
    escape: Escape,
    cursor_visible: bool,
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
        let (columns, rows) = (info.width / CHAR_WIDTH, info.height / LINE_HEIGHT);
        if columns == 0 || rows == 0 {
            return Err(ConsoleError::ScreenTooSmall);
        }
        let needed = info.stride.checked_mul(info.height).and_then(|p| p.checked_mul(info.bytes_per_pixel));
        if needed.is_none_or(|needed| buffer.len() < needed) {
            return Err(ConsoleError::BufferTooSmall);
        }

        let mut console = Console {
            buffer,
            info,
            columns,
            rows,
            row: 0,
            column: 0,
            foreground: Color::GREEN,
            background: Color::BLACK,
            escape: Escape::Text,
            cursor_visible: true,
        };
        console.clear();
        Ok(console)
    }

    pub fn columns(&self) -> usize {
        self.columns
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
                self.clear_cell(self.row, self.column);
                self.draw_glyph(self.row, self.column, c);
                self.column += 1;
            }
        }
    }

    pub fn clear(&mut self) {
        self.escape = Escape::Text;
        self.fill_rect(0, 0, self.info.width, self.info.height, self.background);
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
                for cell in range {
                    self.clear_cell(cell / self.columns, cell % self.columns);
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
        let (x0, y0) = (col * CHAR_WIDTH, row * LINE_HEIGHT);
        (0..CHAR_RASTER_HEIGHT.val()).all(|dy| {
            (0..CHAR_WIDTH).all(|dx| {
                let intensity = glyph_pixel(c, dx, dy);
                let offset = self.pixel_offset(x0 + dx, y0 + dy);
                let (expected, len) = self.encode(background.blend(foreground, intensity));
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
        let line_bytes = LINE_HEIGHT * self.info.stride * self.info.bytes_per_pixel;
        let text_bytes = self.rows * line_bytes;
        self.buffer.copy_within(line_bytes..text_bytes, 0);
        self.fill_rect(0, (self.rows - 1) * LINE_HEIGHT, self.info.width, LINE_HEIGHT, self.background);
    }

    fn clear_cell(&mut self, row: usize, col: usize) {
        self.fill_rect(col * CHAR_WIDTH, row * LINE_HEIGHT, CHAR_WIDTH, LINE_HEIGHT, self.background);
    }

    fn fill_rect(&mut self, x: usize, y: usize, width: usize, height: usize, color: Color) {
        let (bytes, len) = self.encode(color);
        let bytes_per_pixel = self.info.bytes_per_pixel;
        for y in y..(y + height).min(self.info.height) {
            let start = self.pixel_offset(x, y);
            let end = self.pixel_offset((x + width).min(self.info.width), y);
            for pixel in self.buffer[start..end].chunks_exact_mut(bytes_per_pixel) {
                pixel[..len].copy_from_slice(&bytes[..len]);
            }
        }
    }

    fn draw_glyph(&mut self, row: usize, col: usize, c: char) {
        let (x0, y0) = (col * CHAR_WIDTH, row * LINE_HEIGHT);
        for dy in 0..CHAR_RASTER_HEIGHT.val() {
            for dx in 0..CHAR_WIDTH {
                let intensity = glyph_pixel(c, dx, dy);
                let color = self.background.blend(self.foreground, intensity);
                self.write_pixel(x0 + dx, y0 + dy, color);
            }
        }
    }

    fn write_pixel(&mut self, x: usize, y: usize, color: Color) {
        let offset = self.pixel_offset(x, y);
        let (bytes, len) = self.encode(color);
        self.buffer[offset..offset + len].copy_from_slice(&bytes[..len]);
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

fn glyph(c: char) -> RasterizedChar {
    get_raster(c, FONT_WEIGHT, CHAR_RASTER_HEIGHT)
        .unwrap_or_else(|| get_raster(UNKNOWN_CHAR, FONT_WEIGHT, CHAR_RASTER_HEIGHT).unwrap())
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

/// Original 7x9 bitmap for the game's Ж, centered in the existing cell.
fn glyph_pixel(c: char, x: usize, y: usize) -> u8 {
    if c == 'Ж' {
        const ZHE: [u8; 9] = [
            0b1001001, 0b1001001, 0b0101010, 0b0011100, 0b0001000, 0b0011100, 0b0101010, 0b1001001,
            0b1001001,
        ];
        let left = CHAR_WIDTH.saturating_sub(7) / 2;
        if (3..12).contains(&y)
            && (left..left + 7).contains(&x)
            && ZHE[y - 3] & (1 << (6 - (x - left))) != 0
        {
            255
        } else {
            0
        }
    } else {
        glyph(c).raster()[y][x]
    }
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
