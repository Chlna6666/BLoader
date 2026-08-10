use crate::runtime::foundation::{build_info, i18n};

const LOGO_SIZE: usize = 16;
const LOGO: [[u32; LOGO_SIZE]; LOGO_SIZE] = [
    [0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x505040, 0xC0C0A0, 0xC0C0A0, 0x505040, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000],
    [0x000000, 0x000000, 0x000000, 0x000000, 0x606050, 0xC0C0A0, 0xF0F0D0, 0xF0FFE0, 0xF0FFD0, 0xF0FFD0, 0xD0D0B0, 0x606050, 0x000000, 0x000000, 0x000000, 0x000000],
    [0x000000, 0x000000, 0x606050, 0xD0C0A0, 0xF0F0D0, 0xF0FFE0, 0xF0FFE0, 0xFFFFC0, 0xF0FFC0, 0xFFFFF0, 0xFFFFF0, 0xF0FFD0, 0xD0D0B0, 0x707060, 0x000000, 0x000000],
    [0x202020, 0xD0D0B0, 0xF0F0D0, 0xF0FFE0, 0xFFFFF0, 0xFFFFF0, 0xFFFFE0, 0xF0FFB0, 0xF0F0A0, 0xFFFFE0, 0xFFFFF0, 0xFFFFF0, 0xF0FFD0, 0xF0F0C0, 0xD0D0B0, 0x000000],
    [0x404030, 0xD0D0B0, 0xE0E0C0, 0xF0F0D0, 0xF0FFE0, 0xFFFFE0, 0xFFFFD0, 0xFFFFE0, 0xFFFFF0, 0xFFFFE0, 0xFFFFE0, 0xFFFFE0, 0xE0E0C0, 0xB0B0A0, 0x90A080, 0x303020],
    [0x404030, 0xD0D0B0, 0xE0E0D0, 0xE0E0C0, 0xE0E0B0, 0xF0F0D0, 0xFFFFE0, 0xFFFFE0, 0xFFFFE0, 0xFFFFE0, 0xE0E0C0, 0xB0B0A0, 0xA0A090, 0x90A090, 0x90A080, 0x303020],
    [0x404030, 0xD0D0A0, 0xE0E0C0, 0xE0F0E0, 0xE0E0C0, 0xE0E0B0, 0xE0F0C0, 0xF0F0D0, 0xE0E0C0, 0xB0B0A0, 0xA0A090, 0xA0A090, 0x90A080, 0x90A070, 0x909070, 0x303020],
    [0x303030, 0xC0C090, 0xD0D0A0, 0xE0E0C0, 0xE0E0B0, 0xE0E0B0, 0xE0E0C0, 0xE0E0C0, 0xA0A090, 0xA0A090, 0xA0A0A0, 0xA0A090, 0xA0A070, 0x909060, 0x808060, 0x303020],
    [0x303030, 0xB0B090, 0xB0B090, 0xD0D0A0, 0xD0E0A0, 0xD0E090, 0xE0E0C0, 0xD0E0B0, 0xA0A090, 0xA0A090, 0xA0A090, 0xA0A080, 0x90A060, 0x909060, 0x909070, 0x303020],
    [0x303030, 0x807060, 0x807060, 0xC0C090, 0xD0D090, 0xD0D0A0, 0xD0E0A0, 0xD0E0A0, 0xA0A080, 0xA0A080, 0xA0A070, 0x90A060, 0x909060, 0x808060, 0x808060, 0x303020],
    [0x303030, 0xC0C0A0, 0xB0A090, 0xC0B080, 0xC0C090, 0xC0C090, 0xB0B080, 0xD0D0A0, 0x90A080, 0x909060, 0x909060, 0x808060, 0x808060, 0x807060, 0x808060, 0x203020],
    [0x303030, 0xC0C0A0, 0xC0B090, 0xC0B090, 0xC0B080, 0xB0B090, 0x403020, 0xC0C0A0, 0x90A080, 0x808060, 0x808060, 0x807060, 0x807050, 0x808060, 0x909070, 0x203020],
    [0x000000, 0x707060, 0xC0C0A0, 0xB0B090, 0xC0B090, 0xC0C090, 0xA0A080, 0xC0C0A0, 0x909080, 0x808060, 0x808060, 0x808060, 0x908070, 0x808070, 0x505040, 0x000000],
    [0x000000, 0x000000, 0x000000, 0x707060, 0xB0B090, 0xC0C0A0, 0xC0C0A0, 0xC0D0A0, 0x90A080, 0x909070, 0x808070, 0x808070, 0x505040, 0x000000, 0x000000, 0x000000],
    [0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x707060, 0xC0C0A0, 0xC0C0A0, 0x909080, 0x809070, 0x505040, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000],
    [0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x707060, 0x606050, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000],
];

const BLOADER_ART: [&str; 5] = [
    " ____  _                    _           ",
    "| __ )| |    ___   __ _  __| | ___ _ __",
    "|  _ \\| |   / _ \\ / _` |/ _` |/ _ \\ '__|",
    "| |_) | |__| (_) | (_| | (_| |  __/ |   ",
    "|____/|_____\\___/ \\__,_|\\__,_|\\___|_|   ",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BannerLayout {
    Minimal,
    Compact,
    Normal,
    Wide,
}

pub fn choose_layout(columns: usize) -> BannerLayout {
    if columns >= 92 {
        BannerLayout::Wide
    } else if columns >= 68 {
        BannerLayout::Normal
    } else if columns >= 48 {
        BannerLayout::Compact
    } else {
        BannerLayout::Minimal
    }
}

pub fn render_banner(columns: usize, ansi: bool, host_version: &str, mode: &str, debug_destination: &str) -> Vec<String> {
    let layout = choose_layout(columns);
    let mut lines = Vec::new();

    match layout {
        BannerLayout::Minimal => {
            lines.push(color_text("BLoader", 117, 214, 255, ansi));
        }
        BannerLayout::Compact => {
            for (index, art) in BLOADER_ART.iter().enumerate() {
                let (r, g, b) = wordmark_color(index);
                lines.push(color_text(art, r, g, b, ansi));
            }
        }
        BannerLayout::Normal | BannerLayout::Wide => {
            let logo_rows = if layout == BannerLayout::Wide { 12 } else { 10 };
            let logo = render_logo(logo_rows, ansi);
            let height = logo.len().max(BLOADER_ART.len());
            for row in 0..height {
                let left = logo.get(row).cloned().unwrap_or_else(|| " ".repeat(logo_rows * 2));
                let right = BLOADER_ART.get(row).copied().unwrap_or("");
                let (r, g, b) = wordmark_color(row.min(BLOADER_ART.len() - 1));
                let right = color_text(right, r, g, b, ansi);
                lines.push(format!("{left}  {right}"));
            }
        }
    }

    let subtitle = i18n::tr("console.banner.subtitle");
    let debug_label = i18n::tr("console.banner.full_debug");
    let mode_label = i18n::tr("console.banner.file_io");
    lines.push(String::new());

    if layout == BannerLayout::Minimal {
        lines.push(fit_plain(&format!("v{} | Minecraft {}", build_info::VERSION, host_version), columns));
        lines.push(fit_plain(&subtitle, columns));
    } else {
        lines.push(fit_plain(
            &format!("BLoader v{}  |  Minecraft {}  |  {}", build_info::VERSION, host_version, build_info::LICENSE),
            columns,
        ));
        lines.push(fit_plain(&subtitle, columns));
        lines.push(fit_plain(&format!("{mode_label}: {mode}"), columns));
        lines.push(fit_plain(&format!("{debug_label}: {debug_destination}"), columns));
    }
    lines.push(String::new());
    lines
}

fn render_logo(target_rows: usize, ansi: bool) -> Vec<String> {
    if !ansi || target_rows == 0 {
        return Vec::new();
    }
    let target_rows = target_rows.clamp(6, LOGO_SIZE);
    let mut rows = Vec::with_capacity(target_rows);
    for out_y in 0..target_rows {
        let src_y = out_y * LOGO_SIZE / target_rows;
        let mut line = String::new();
        let mut active_color: Option<u32> = None;
        for out_x in 0..target_rows {
            let src_x = out_x * LOGO_SIZE / target_rows;
            let color = LOGO[src_y][src_x];
            if color == 0 {
                if active_color.take().is_some() {
                    line.push_str("\x1b[0m");
                }
                line.push_str("  ");
                continue;
            }
            if active_color != Some(color) {
                let r = (color >> 16) & 0xff;
                let g = (color >> 8) & 0xff;
                let b = color & 0xff;
                line.push_str(&format!("\x1b[48;2;{r};{g};{b}m"));
                active_color = Some(color);
            }
            line.push_str("  ");
        }
        if active_color.is_some() {
            line.push_str("\x1b[0m");
        }
        rows.push(line);
    }
    rows
}

fn wordmark_color(row: usize) -> (u8, u8, u8) {
    const COLORS: [(u8, u8, u8); 5] = [
        (117, 214, 255),
        (104, 196, 255),
        (129, 166, 255),
        (168, 145, 255),
        (201, 132, 255),
    ];
    COLORS[row.min(COLORS.len() - 1)]
}

fn color_text(text: &str, r: u8, g: u8, b: u8, ansi: bool) -> String {
    if ansi {
        format!("\x1b[38;2;{r};{g};{b}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

fn fit_plain(text: &str, columns: usize) -> String {
    if columns == 0 {
        return String::new();
    }
    let mut width = 0usize;
    let mut output = String::new();
    for ch in text.chars() {
        let char_width = if ch.is_ascii() { 1 } else { 2 };
        if width + char_width > columns.saturating_sub(1) {
            if columns >= 4 {
                output.push_str("...");
            }
            break;
        }
        output.push(ch);
        width += char_width;
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_never_forces_art_into_narrow_console() {
        assert_eq!(choose_layout(40), BannerLayout::Minimal);
        assert_eq!(choose_layout(50), BannerLayout::Compact);
        assert_eq!(choose_layout(70), BannerLayout::Normal);
        assert_eq!(choose_layout(100), BannerLayout::Wide);
    }

    #[test]
    fn ascii_wordmark_is_portable() {
        assert!(BLOADER_ART.iter().all(|line| line.is_ascii()));
    }

    #[test]
    fn narrow_metadata_is_clamped() {
        assert!(fit_plain("abcdefghijklmnopqrstuvwxyz", 10).len() <= 12);
    }
}
