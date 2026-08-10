const LOGO_SIZE: usize = 20;
const LOGO: [[u32; LOGO_SIZE]; LOGO_SIZE] = [
    [0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x5F5F52, 0xD1D2B2, 0xCCCFB3, 0x59594E, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000],
    [0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x5E5E4F, 0xC4C4A5, 0xF9FAD8, 0xF7FCE0, 0xFCFFD7, 0xFCFFD6, 0xC5C6A9, 0x5E5F52, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000],
    [0x000000, 0x000000, 0x000000, 0x000000, 0x656452, 0xC7C8A4, 0xF7F9CB, 0xF9FFD1, 0xF7FBE3, 0xFAFEE9, 0xF4F9D3, 0xF5FADF, 0xFFFFF3, 0xFFFFE9, 0xD6D6BA, 0x6F7061, 0x121211, 0x000000, 0x000000, 0x000000],
    [0x000000, 0x151512, 0x706F5C, 0xCECCA8, 0xFCFBD4, 0xFCFEDF, 0xF5F9E2, 0xF8FBE9, 0xF9FDD8, 0xF9F895, 0xF7F79D, 0xF8FCD6, 0xFAFBEE, 0xF6F9E5, 0xFDFFD8, 0xFFFFCF, 0xDBDDB5, 0x7E7F6A, 0x20201C, 0x000000],
    [0x191916, 0xC4C5A7, 0xFFFFD5, 0xFCFCD7, 0xF6F9DC, 0xFBFCE9, 0xFFFEF6, 0xFFFEF8, 0xF9FAD0, 0xF5F8AD, 0xF6F7A4, 0xF6F9C2, 0xF9FDED, 0xFBFDF1, 0xF8FBEC, 0xF6F8DC, 0xFFFFD3, 0xFFFFD8, 0xB2B598, 0x121310],
    [0x23241E, 0xC7CDAC, 0xD6DBB9, 0xE8EBC8, 0xF5F8D7, 0xF9FCDF, 0xFBFCE3, 0xF9FCE3, 0xF7F9CB, 0xF8FCE1, 0xFCFEF4, 0xFBFDEC, 0xF7FADE, 0xFAFCE0, 0xFFFFE5, 0xF5F5D6, 0xC9CCAE, 0x9AA086, 0x8B9278, 0x1A1B16],
    [0x1F201B, 0xC4C9AA, 0xD8DDBF, 0xDBE0C4, 0xE0E5C2, 0xE6E9B5, 0xF3F4C3, 0xFAFCD9, 0xFEFDE6, 0xFAFCE5, 0xF7FAE1, 0xFEFEE6, 0xFFFFE3, 0xF0F1C8, 0xCBCCAB, 0xA3A894, 0x909989, 0x8F9881, 0x8C9379, 0x181915],
    [0x1E1F1B, 0xC3C8A2, 0xD8E0B8, 0xE1E8D8, 0xE6EBD9, 0xDCE2B7, 0xDCE1AB, 0xECEFCF, 0xF7F7D9, 0xFBFCDC, 0xFDFED8, 0xEFEFC6, 0xCBCCAF, 0xA6A896, 0x969B8E, 0x96A094, 0x97A28A, 0x969F7D, 0x8C9379, 0x171814],
    [0x1E1F1B, 0xB9BA8F, 0xD0D49D, 0xDCE6CE, 0xE2EBDF, 0xDEE6C5, 0xDAE0A8, 0xDEE4BD, 0xE2E6C4, 0xEAEDCC, 0xCACCB2, 0xA0A48F, 0x989C8D, 0x9CA399, 0x99A493, 0x959E7C, 0x959A61, 0x8E9361, 0x868A6E, 0x181914],
    [0x1D1E1A, 0xB8BA94, 0xCACA8E, 0xD4D8A0, 0xDBE4C0, 0xD9DFAD, 0xDBE1AF, 0xDDE5BE, 0xE0E7C8, 0xD6DCB6, 0x9BA18A, 0x9BA191, 0xA1A79A, 0x9BA69B, 0x97A28C, 0x9BA37A, 0x95995F, 0x8A8C61, 0x838669, 0x181915],
    [0x1E1F1A, 0xB5B596, 0xB7B594, 0xC5C589, 0xD6D99E, 0xD9DFAB, 0xD5D68D, 0xDAE1AF, 0xDDE6C6, 0xD6DBB2, 0xA1A990, 0x9AA395, 0x9BA79C, 0x99A287, 0x99A179, 0x989F6F, 0x8E9161, 0x898A66, 0x8C8E76, 0x171813],
    [0x22241F, 0x9E9D81, 0x473527, 0xA4A180, 0xCECC90, 0xD1D393, 0xD4D89C, 0xD5D99B, 0xD6DDAB, 0xD2D7A9, 0x9EA68A, 0x969E84, 0x9AA283, 0x99A073, 0x999D6B, 0x8D8F5D, 0x83855D, 0x807B5C, 0x87846C, 0x171814],
    [0x1E1E1A, 0xAFB094, 0x5F5747, 0xA19C7F, 0xC5C191, 0xC9C78B, 0xCDCE90, 0xD3D597, 0xDBE0A7, 0xD0D5A7, 0x9BA184, 0x919368, 0x999C68, 0x9BA062, 0x8D8F5F, 0x878662, 0x827F60, 0x796D54, 0x838169, 0x171914],
    [0x191A16, 0xBDBEA0, 0xCCC5A5, 0xC0B496, 0xB9A887, 0xBCB585, 0xCDCD95, 0x918468, 0x999677, 0xD1D7AC, 0x9AA185, 0x8E906C, 0x8F9162, 0x8C8E62, 0x80805C, 0x7E7559, 0x7D6F57, 0x81785E, 0x86876F, 0x161713],
    [0x1B1C18, 0xC4C8A9, 0xC9C3A3, 0xBEAE8E, 0xC2B090, 0xB9A985, 0xCFCCA3, 0x6A5F4D, 0x46392B, 0xCCD2AC, 0x9BA186, 0x87896B, 0x828261, 0x858062, 0x82765B, 0x7B6D55, 0x827860, 0x8B896F, 0x8F927A, 0x161713],
    [0x000000, 0x686A5A, 0xBABD9F, 0xD0CFAC, 0xB7AF91, 0xB7A98B, 0xC7B897, 0xB7AE90, 0x959179, 0xC9CCA9, 0x979B82, 0x858367, 0x858263, 0x81745B, 0x83785F, 0x89856C, 0x95977E, 0x838770, 0x494C3F, 0x000000],
    [0x000000, 0x000000, 0x181814, 0x737563, 0x96987F, 0xA7A084, 0xCBC09F, 0xC3B595, 0xCEC6A5, 0xC7CBA9, 0x989D86, 0x8C8C74, 0x847E65, 0x827B62, 0x929075, 0x878A73, 0x4C4F42, 0x000000, 0x000000, 0x000000],
    [0x000000, 0x000000, 0x000000, 0x000000, 0x1C1D18, 0x717361, 0xBDBE9F, 0xD4D3B1, 0xC8C6A6, 0xC0C3A3, 0x949982, 0x8A8E78, 0x95987F, 0x878B75, 0x515446, 0x11120F, 0x000000, 0x000000, 0x000000, 0x000000],
    [0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x191915, 0x6C6E5C, 0xB8BA9C, 0xC8CCAA, 0x9A9F87, 0x7F846D, 0x4D4F42, 0x12120F, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000],
    [0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x1A1B17, 0x787A67, 0x626555, 0x141411, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000, 0x000000],
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
    if columns >= 100 {
        BannerLayout::Wide
    } else if columns >= 82 {
        BannerLayout::Normal
    } else if columns >= 48 {
        BannerLayout::Compact
    } else {
        BannerLayout::Minimal
    }
}

pub fn render_banner(columns: usize, ansi: bool) -> Vec<String> {
    let layout = choose_layout(columns);
    match layout {
        BannerLayout::Minimal => vec![color_text("BLoader", 117, 214, 255, ansi), String::new()],
        BannerLayout::Compact => BLOADER_ART
            .iter()
            .enumerate()
            .map(|(index, art)| {
                let (r, g, b) = wordmark_color(index);
                color_text(art, r, g, b, ansi)
            })
            .chain(std::iter::once(String::new()))
            .collect(),
        BannerLayout::Normal | BannerLayout::Wide => {
            let logo_rows = if layout == BannerLayout::Wide { 20 } else { 16 };
            compose_centered(render_logo(logo_rows, ansi), ansi)
        }
    }
}

fn compose_centered(logo: Vec<String>, ansi: bool) -> Vec<String> {
    let logo_height = logo.len();
    let art_height = BLOADER_ART.len();
    let height = logo_height.max(art_height);
    let logo_top = (height - logo_height) / 2;
    let art_top = (height - art_height) / 2;
    let logo_width = logo_height * 2;
    let mut lines = Vec::with_capacity(height + 1);

    for row in 0..height {
        let left = if row >= logo_top && row < logo_top + logo_height {
            logo[row - logo_top].clone()
        } else {
            " ".repeat(logo_width)
        };
        let right = if row >= art_top && row < art_top + art_height {
            let art_row = row - art_top;
            let (r, g, b) = wordmark_color(art_row);
            color_text(BLOADER_ART[art_row], r, g, b, ansi)
        } else {
            String::new()
        };
        lines.push(format!("{left}  {right}"));
    }
    lines.push(String::new());
    lines
}

fn render_logo(target_rows: usize, ansi: bool) -> Vec<String> {
    if !ansi || target_rows == 0 {
        return Vec::new();
    }
    let target_rows = target_rows.clamp(8, LOGO_SIZE);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_avoids_wrapping() {
        assert_eq!(choose_layout(40), BannerLayout::Minimal);
        assert_eq!(choose_layout(50), BannerLayout::Compact);
        assert_eq!(choose_layout(90), BannerLayout::Normal);
        assert_eq!(choose_layout(120), BannerLayout::Wide);
    }

    #[test]
    fn ascii_wordmark_is_portable() {
        assert!(BLOADER_ART.iter().all(|line| line.is_ascii()));
    }

    #[test]
    fn centered_art_starts_below_tall_logo_top() {
        let lines = compose_centered(vec!["x".into(); 16], false);
        let first_art = lines.iter().position(|line| line.contains("____")).unwrap();
        assert!(first_art >= 5);
    }
}
