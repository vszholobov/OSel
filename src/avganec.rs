//! aVGAnec module provides basic text showing methods on top of the linear framebuffer
//!
//! Works the same way under UEFI (GOP) and BIOS (VESA): glyphs are rasterized
//! with the Noto Sans Mono bitmap font and drawn pixel by pixel.

use bootloader_api::info::{FrameBuffer, FrameBufferInfo, PixelFormat};
use core::fmt;
use core::panic::PanicInfo;
use noto_sans_mono_bitmap::{FontWeight, RasterHeight, RasterizedChar, get_raster, get_raster_width};
use spin::Mutex;

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::avganec::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    // an interrupt handler printing while we hold the lock would deadlock
    x86_64::instructions::interrupts::without_interrupts(|| {
        // printing before `init` or without a framebuffer is a no-op
        if let Some(writer) = WRITER.lock().as_mut() {
            writer.write_fmt(args).unwrap();
        }
    });
}

/// Shows a fatal error white on red over the whole screen and mirrors it to the serial port
pub fn panic_screen(info: &PanicInfo) {
    use core::fmt::Write;
    x86_64::instructions::interrupts::disable();
    // the panic could happen while printing, nobody would release these locks anymore
    unsafe {
        WRITER.force_unlock();
        crate::cereal::SERIAL1.force_unlock();
    }
    if let Some(writer) = WRITER.lock().as_mut() {
        writer.set_colors(Color::WHITE, Color::RED);
        writer.clear();
        let _ = write!(writer, "OSel crashed :(\n\n{}\n", info);
    }
    crate::serial_println!("OSel crashed: {}", info);
}

/// Color of the pixel at (`x`, `y`), `None` without a framebuffer
pub fn read_pixel(x: usize, y: usize) -> Option<Color> {
    let writer = WRITER.lock();
    let writer = writer.as_ref()?;
    let offset = writer.pixel_offset(x, y);
    let bytes = &writer.buffer[offset..offset + writer.info.bytes_per_pixel];
    Some(match writer.info.pixel_format {
        PixelFormat::Rgb => Color::new(bytes[0], bytes[1], bytes[2]),
        PixelFormat::Bgr => Color::new(bytes[2], bytes[1], bytes[0]),
        _ => Color::new(bytes[0], bytes[0], bytes[0]),
    })
}

const FONT_WEIGHT: FontWeight = FontWeight::Regular;
const CHAR_RASTER_HEIGHT: RasterHeight = RasterHeight::Size16;
const CHAR_WIDTH: usize = get_raster_width(FONT_WEIGHT, CHAR_RASTER_HEIGHT);
const LINE_HEIGHT: usize = CHAR_RASTER_HEIGHT.val() + 2;
const UNKNOWN_AVGANEC: char = '\u{FFFD}';

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
    fn blend(self, fg: Color, intensity: u8) -> Color {
        let mix = |bg: u8, fg: u8| ((bg as u16 * (255 - intensity as u16) + fg as u16 * intensity as u16) / 255) as u8;
        Color::new(mix(self.r, fg.r), mix(self.g, fg.g), mix(self.b, fg.b))
    }
}

pub static WRITER: Mutex<Option<Writer>> = Mutex::new(None);

/// Takes over the framebuffer handed over by the bootloader
pub fn init(framebuffer: Option<FrameBuffer>) {
    let writer = framebuffer.map(|fb| {
        let info = fb.info();
        let mut writer = Writer {
            buffer: fb.into_buffer(),
            info,
            columns: info.width / CHAR_WIDTH,
            rows: info.height / LINE_HEIGHT,
            row_position: 0,
            column_position: 0,
            foreground: Color::GREEN,
            background: Color::BLACK,
        };
        writer.clear();
        writer
    });
    *WRITER.lock() = writer;
}

/// Text console over the framebuffer. Text goes from the top down, once the cursor
/// reaches the bottom row the screen scrolls up on every new line.
pub struct Writer {
    buffer: &'static mut [u8],
    info: FrameBufferInfo,
    columns: usize,
    rows: usize,
    row_position: usize,
    column_position: usize,
    foreground: Color,
    background: Color,
}

impl fmt::Write for Writer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_string(s);
        Ok(())
    }
}

impl Writer {
    pub fn write_string(&mut self, s: &str) {
        for c in s.chars() {
            self.write_char(c);
        }
    }

    pub fn write_char(&mut self, c: char) {
        match c {
            '\n' => self.new_line(),
            '\r' => self.column_position = 0,
            c => {
                if self.column_position >= self.columns {
                    self.new_line();
                }
                let row = self.row_position;
                let col = self.column_position;
                self.draw_glyph(row, col, glyph(c));
                self.column_position += 1;
            }
        }
    }

    pub fn set_colors(&mut self, foreground: Color, background: Color) {
        self.foreground = foreground;
        self.background = background;
    }

    pub fn clear(&mut self) {
        for y in 0..self.info.height {
            self.fill_pixel_row(y, self.background);
        }
        self.row_position = 0;
        self.column_position = 0;
    }

    fn new_line(&mut self) {
        self.column_position = 0;
        if self.row_position + 1 < self.rows {
            self.row_position += 1;
            return;
        }
        let line_bytes = LINE_HEIGHT * self.info.stride * self.info.bytes_per_pixel;
        let text_bytes = self.rows * line_bytes;
        self.buffer.copy_within(line_bytes..text_bytes, 0);
        self.clear_row(self.rows - 1);
    }

    fn clear_row(&mut self, row: usize) {
        for y in row * LINE_HEIGHT..(row + 1) * LINE_HEIGHT {
            self.fill_pixel_row(y, self.background);
        }
    }

    fn fill_pixel_row(&mut self, y: usize, color: Color) {
        for x in 0..self.info.width {
            self.write_pixel(x, y, color);
        }
    }

    fn draw_glyph(&mut self, row: usize, col: usize, glyph: RasterizedChar) {
        let (x0, y0) = (col * CHAR_WIDTH, row * LINE_HEIGHT);
        for (dy, line) in glyph.raster().iter().enumerate() {
            for (dx, &intensity) in line.iter().enumerate() {
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
        let bytes = match self.info.pixel_format {
            PixelFormat::Rgb => [color.r, color.g, color.b, 0],
            PixelFormat::Bgr => [color.b, color.g, color.r, 0],
            PixelFormat::U8 => [((color.r as u16 + color.g as u16 + color.b as u16) / 3) as u8, 0, 0, 0],
            other => panic!("unsupported framebuffer pixel format {:?}", other),
        };
        (bytes, self.info.bytes_per_pixel.min(bytes.len()))
    }

    /// Checks that the text cell at (`row`, `col`) shows exactly the glyph of `c`
    #[cfg(test)]
    fn cell_shows(&self, row: usize, col: usize, c: char) -> bool {
        let (x0, y0) = (col * CHAR_WIDTH, row * LINE_HEIGHT);
        glyph(c).raster().iter().enumerate().all(|(dy, line)| {
            line.iter().enumerate().all(|(dx, &intensity)| {
                let offset = self.pixel_offset(x0 + dx, y0 + dy);
                let (expected, len) = self.encode(self.background.blend(self.foreground, intensity));
                self.buffer[offset..offset + len] == expected[..len]
            })
        })
    }
}

fn glyph(c: char) -> RasterizedChar {
    get_raster(c, FONT_WEIGHT, CHAR_RASTER_HEIGHT)
        .unwrap_or_else(|| get_raster(UNKNOWN_AVGANEC, FONT_WEIGHT, CHAR_RASTER_HEIGHT).unwrap())
}

#[test_case]
fn test_println_simple() {
    println!("test_println_simple output");
}

#[test_case]
fn test_println_many() {
    for _ in 0..200 {
        println!("test_println_many output");
    }
}

#[test_case]
fn test_println_output() {
    let s = "Some test string that fits on a single line";
    println!("{}", s);
    let writer = WRITER.lock();
    let writer = writer.as_ref().expect("no framebuffer");
    // println moved the cursor to the start of the next line
    let row = writer.row_position - 1;
    for (i, c) in s.chars().enumerate() {
        assert!(writer.cell_shows(row, i, c), "wrong glyph for {:?} at column {}", c, i);
    }
}
